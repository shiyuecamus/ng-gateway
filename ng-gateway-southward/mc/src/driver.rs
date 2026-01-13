use crate::{
    codec::McCodec,
    protocol::{
        frame::addr::McLogicalAddress,
        planner::{PlannerConfig, WriteEntry},
    },
    supervisor::{McSupervisor, SessionEntry, SharedSession},
    typed_api::{
        McReadItemTyped, McTypedApi, TypedMultiAddressReadSpec, TypedMultiAddressWriteSpec,
        TypedPointReadSpec,
    },
    types::{McAction, McChannel, McDevice, McPoint},
};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, AttributeData, DataPointType, DataType, Driver, DriverError,
    DriverHealth, DriverResult, ExecuteOutcome, ExecuteResult, HealthStatus, NGValue,
    NGValueCastError, NorthwardData, PointValue, RuntimeAction, RuntimeDevice, RuntimeParameter,
    RuntimePoint, SouthwardConnectionState, SouthwardInitContext, TelemetryData, WriteOutcome,
    WriteResult,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration as StdDuration, Instant},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

/// MC driver state and metrics.
///
/// This struct keeps minimal state required by the gateway driver trait and
/// integrates with `McSupervisor` for connection management.
pub struct McDriver {
    /// Raw driver configuration (typed in factory).
    inner: Arc<McChannel>,
    /// Shared session entry with underlying IO handle and health flags.
    pub(super) shared: SharedSession,
    /// Started flag to guard duplicate `start()` calls.
    started: AtomicBool,
    /// Cancel token used to stop supervisor and internal tasks.
    cancel_token: CancellationToken,
    /// Moving counters for quick health.
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    last_avg_response_time_ms: AtomicU64,
    /// Connection state broadcast channel.
    conn_tx: watch::Sender<SouthwardConnectionState>,
    conn_rx: watch::Receiver<SouthwardConnectionState>,
}

impl McDriver {
    /// Create a new MC driver from initialization context.
    ///
    /// This performs type downcast checks and initializes shared session state
    /// but does not perform any I/O. All network activity is started in `start()`.
    pub fn with_context(ctx: SouthwardInitContext) -> DriverResult<Self> {
        let (tx, rx) = watch::channel(SouthwardConnectionState::Disconnected);

        let inner = ctx
            .runtime_channel
            .downcast_arc::<McChannel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid McChannel".to_string()))?;

        Ok(Self {
            inner,
            shared: Arc::new(SessionEntry::new_empty()),
            started: AtomicBool::new(false),
            cancel_token: CancellationToken::new(),
            conn_tx: tx,
            conn_rx: rx,
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            last_avg_response_time_ms: AtomicU64::new(0),
        })
    }

    /// Execute a typed multi-address read operation.
    ///
    /// This helper exposes the `McTypedApi::read_multi_address_typed` facade
    /// through the driver, handling session acquisition and metrics updates.
    /// Callers must provide a list of fully-resolved typed multi-address
    /// specifications including `DataType` and logical addresses.
    #[allow(unused)]
    pub async fn read_multi_address_typed(
        &self,
        specs: Vec<TypedMultiAddressReadSpec>,
    ) -> DriverResult<Vec<McReadItemTyped>> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        let session = self
            .shared
            .session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)?;

        let start_ts = Instant::now();
        let result = McTypedApi::read_multi_address_typed(&session, specs).await;

        match result {
            Ok(values) => {
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                let elapsed_ms = start_ts.elapsed().as_millis() as u64;
                self.successful_requests.fetch_add(1, Ordering::Relaxed);
                let prev = self.last_avg_response_time_ms.load(Ordering::Relaxed);
                let new_avg = if prev == 0 {
                    elapsed_ms
                } else {
                    (prev.saturating_mul(9) + elapsed_ms) / 10
                };
                self.last_avg_response_time_ms
                    .store(new_avg, Ordering::Relaxed);
                Ok(values)
            }
            Err(e) => {
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
                Err(DriverError::ExecutionError(e.to_string()))
            }
        }
    }

    /// Execute a typed multi-address write operation.
    ///
    /// This helper exposes the `McTypedApi::write_multi_address_typed` facade
    /// through the driver, handling session acquisition and metrics updates.
    /// Callers must provide a list of fully-resolved typed multi-address
    /// write specifications where values are expressed as JSON.
    #[allow(unused)]
    pub async fn write_multi_address_typed(
        &self,
        specs: Vec<TypedMultiAddressWriteSpec>,
    ) -> DriverResult<()> {
        if specs.is_empty() {
            return Ok(());
        }

        let session = self
            .shared
            .session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)?;

        let start_ts = Instant::now();
        if let Err(e) = McTypedApi::write_multi_address_typed(&session, specs).await {
            self.total_requests.fetch_add(1, Ordering::Relaxed);
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
            return Err(DriverError::ExecutionError(e.to_string()));
        }

        self.total_requests.fetch_add(1, Ordering::Relaxed);
        let elapsed_ms = start_ts.elapsed().as_millis() as u64;
        self.successful_requests.fetch_add(1, Ordering::Relaxed);
        let prev = self.last_avg_response_time_ms.load(Ordering::Relaxed);
        let new_avg = if prev == 0 {
            elapsed_ms
        } else {
            (prev.saturating_mul(9) + elapsed_ms) / 10
        };
        self.last_avg_response_time_ms
            .store(new_avg, Ordering::Relaxed);

        Ok(())
    }
}

/// Compute word-length (MC "points") for a given data type. For string values,
/// `string_len_bytes` controls how many bytes are read and is rounded up to
/// whole words.
fn words_for_data_type(data_type: DataType, string_len_bytes: Option<u16>) -> DriverResult<u16> {
    let words = match data_type {
        DataType::Boolean => 1,
        DataType::Int8 | DataType::UInt8 => 1,
        DataType::Int16 | DataType::UInt16 => 1,
        DataType::Int32 | DataType::UInt32 | DataType::Float32 => 2,
        DataType::Int64 | DataType::UInt64 | DataType::Float64 => 4,
        DataType::String => {
            let bytes = string_len_bytes.ok_or(DriverError::ConfigurationError(
                "MC String point requires stringLenBytes configuration".to_string(),
            ))?;
            (bytes as u32).div_ceil(2) as u16
        }
        DataType::Binary | DataType::Timestamp => {
            return Err(DriverError::ConfigurationError(
                "MC Binary/Timestamp data types are not supported yet".to_string(),
            ))
        }
    };
    Ok(words.max(1))
}

#[async_trait]
impl Driver for McDriver {
    /// Start the MC driver by spawning a supervisor for the underlying transport.
    #[inline]
    #[instrument(level = "info", skip_all)]
    async fn start(&self) -> DriverResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let cancel = self.cancel_token.child_token();
        let shared = Arc::clone(&self.shared);
        let supervisor = McSupervisor::new(shared, cancel, self.conn_tx.clone());
        supervisor.run(Arc::clone(&self.inner)).await?;
        Ok(())
    }

    /// Stop the MC driver and cancel reconnection attempts.
    #[inline]
    #[instrument(level = "info", skip_all)]
    async fn stop(&self) -> DriverResult<()> {
        self.cancel_token.cancel();
        self.started.store(false, Ordering::Release);
        Ok(())
    }

    /// Collect data from specified points.
    ///
    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn collect_data(
        &self,
        device: Arc<dyn RuntimeDevice>,
        data_points: Arc<[Arc<dyn RuntimePoint>]>,
    ) -> DriverResult<Vec<NorthwardData>> {
        let md = device
            .downcast_ref::<McDevice>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimeDevice is not McDevice for McDriver".to_string(),
            ))?;

        // Downcast points to McPoint and collect readable ones.
        let mc_points: Vec<Arc<McPoint>> = data_points
            .iter()
            .filter_map(|p| Arc::clone(p).downcast_arc::<McPoint>().ok())
            .filter(|p| matches!(p.access_mode, AccessMode::Read | AccessMode::ReadWrite))
            .collect();

        if mc_points.is_empty() {
            return Ok(Vec::new());
        }

        // Acquire active session (single connection per channel).
        let session = self
            .shared
            .session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)?;

        let mut specs: Vec<TypedPointReadSpec> = Vec::with_capacity(mc_points.len());
        for (index, point) in mc_points.iter().enumerate() {
            let addr = point
                .address
                .logical
                .clone()
                .ok_or(DriverError::ConfigurationError(format!(
                    "MC logical address not resolved for point '{}'",
                    point.key
                )))?;

            if addr.device.is_bit() {
                // Bit devices currently only support Boolean data type.
                if point.data_type != DataType::Boolean {
                    return Err(DriverError::ConfigurationError(format!(
                        "MC bit device {:?} only supports Boolean data type for point '{}'",
                        addr.device, point.key
                    )));
                }
                // For now we only support pure bit devices, not bit-indexing
                // into word devices such as D20.2.
                if addr.bit.is_some() {
                    return Err(DriverError::ConfigurationError(format!(
                        "Bit-indexed MC address '{}' is not supported yet for collect_data",
                        point.address.raw
                    )));
                }

                let device_code =
                    addr.device
                        .device_code_3e()
                        .ok_or(DriverError::ConfigurationError(format!(
                            "Unsupported MC bit device type for batch read: {:?}",
                            addr.device
                        )))?;

                specs.push(TypedPointReadSpec {
                    index,
                    data_type: point.data_type,
                    addr,
                    // For bit devices, `word_len` is interpreted as the number
                    // of logical points (bits) instead of 16-bit words.
                    word_len: 1,
                    device_code,
                });
            } else {
                // Word/dword devices without per-bit indexing.
                if addr.bit.is_some() || (!addr.device.is_word() && !addr.device.is_dword()) {
                    return Err(DriverError::ConfigurationError(format!(
                        "Unsupported MC address for collect_data: '{}'",
                        point.address.raw
                    )));
                }

                // Enforce MC specification restrictions for batch word read
                // (forbidden long timer/index devices).
                if addr.device.is_forbidden_batch_word_read() {
                    return Err(DriverError::ConfigurationError(format!(
                        "MC device type {:?} is not allowed for batch word read (point '{}')",
                        addr.device, point.key
                    )));
                }

                let word_len = words_for_data_type(point.data_type, point.string_len_bytes)?;
                let device_code =
                    addr.device
                        .device_code_3e()
                        .ok_or(DriverError::ConfigurationError(format!(
                            "Unsupported MC device type for batch read: {:?}",
                            addr.device
                        )))?;

                specs.push(TypedPointReadSpec {
                    index,
                    data_type: point.data_type,
                    addr,
                    word_len,
                    device_code,
                });
            }
        }

        let mut telemetry_values: Vec<PointValue> = Vec::with_capacity(mc_points.len());
        let mut attribute_values: Vec<PointValue> = Vec::with_capacity(mc_points.len());

        // Execute typed read via session; update metrics once per collect cycle,
        // aligned with S7 driver behaviour.
        let series_max = self.inner.config.series.device_batch_in_word_points_max();
        let max_points = self
            .inner
            .config
            .max_points_per_batch
            .unwrap_or(series_max)
            .max(1);
        let max_bytes = self.inner.config.max_bytes_per_frame.unwrap_or(4096).max(1);
        let start_ts = Instant::now();
        let read_results = match McTypedApi::read_points_typed(
            &session,
            &PlannerConfig::new(max_points, max_bytes),
            specs,
        )
        .await
        {
            Ok(values) => {
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                let elapsed_ms = start_ts.elapsed().as_millis() as u64;
                self.successful_requests.fetch_add(1, Ordering::Relaxed);
                let prev = self.last_avg_response_time_ms.load(Ordering::Relaxed);
                let new_avg = if prev == 0 {
                    elapsed_ms
                } else {
                    (prev.saturating_mul(9) + elapsed_ms) / 10
                };
                self.last_avg_response_time_ms
                    .store(new_avg, Ordering::Relaxed);
                values
            }
            Err(e) => {
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
                return Err(DriverError::ExecutionError(e.to_string()));
            }
        };

        for item in read_results.into_iter() {
            if item.end_code != 0 {
                continue;
            }
            if item.index >= mc_points.len() {
                continue;
            }
            let point = &mc_points[item.index];
            let Some(value) = item.value else {
                continue;
            };

            let target = match point.r#type {
                DataPointType::Telemetry => &mut telemetry_values,
                DataPointType::Attribute => &mut attribute_values,
            };
            target.push(PointValue {
                point_id: point.id,
                point_key: Arc::<str>::from(point.key.as_str()),
                value,
            });
        }

        let mut out = Vec::with_capacity(2);
        if !telemetry_values.is_empty() {
            out.push(NorthwardData::Telemetry(TelemetryData::new(
                md.id,
                md.device_name.clone(),
                telemetry_values,
            )));
        }
        if !attribute_values.is_empty() {
            out.push(NorthwardData::Attributes(
                AttributeData::new_client_attributes(
                    md.id,
                    md.device_name.clone(),
                    attribute_values,
                ),
            ));
        }

        Ok(out)
    }

    /// Execute an action/command.
    ///
    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let action = action
            .downcast_ref::<McAction>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimeAction is not McAction".to_string(),
            ))?;
        let device = device
            .downcast_ref::<McDevice>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimeDevice is not McDevice".to_string(),
            ))?;

        let resolved = downcast_parameters::<crate::types::McParameter>(parameters)?;

        // Acquire active session.
        let session = self
            .shared
            .session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)?;

        let mut entries = Vec::with_capacity(resolved.len());

        for (param, value) in resolved.iter() {
            let addr = McLogicalAddress::parse(&param.address.raw).map_err(|e| {
                DriverError::ConfigurationError(format!(
                    "Invalid MC address '{}' in action parameter: {e}",
                    param.address.raw
                ))
            })?;

            let (word_len, device_code) = if addr.device.is_bit() {
                // Bit devices currently only support Boolean data type for
                // actions.
                if param.data_type != DataType::Boolean {
                    return Err(DriverError::ConfigurationError(format!(
                        "MC bit device {:?} only supports Boolean data type in action '{}'",
                        addr.device, action.name
                    )));
                }
                if addr.bit.is_some() {
                    return Err(DriverError::ConfigurationError(format!(
                        "Bit-indexed MC address '{}' is not supported yet for execute_action",
                        param.address.raw
                    )));
                }
                let device_code =
                    addr.device
                        .device_code_3e()
                        .ok_or(DriverError::ConfigurationError(format!(
                            "Unsupported MC bit device type for batch write: {:?}",
                            addr.device
                        )))?;
                (1, device_code)
            } else {
                if addr.bit.is_some() || (!addr.device.is_word() && !addr.device.is_dword()) {
                    return Err(DriverError::ConfigurationError(format!(
                        "Unsupported MC address for execute_action: '{}'",
                        param.address.raw
                    )));
                }

                // Enforce MC specification restrictions for batch word write
                // (forbidden long timer/index devices).
                if addr.device.is_forbidden_batch_word_write() {
                    return Err(DriverError::ConfigurationError(format!(
                        "MC device type {:?} is not allowed for batch word write (action '{}')",
                        addr.device, action.name
                    )));
                }

                let word_len = words_for_data_type(param.data_type, param.string_len_bytes)?;
                let device_code =
                    addr.device
                        .device_code_3e()
                        .ok_or(DriverError::ConfigurationError(format!(
                            "Unsupported MC device type for batch write: {:?}",
                            addr.device
                        )))?;
                (word_len, device_code)
            };

            let data_bytes = McCodec::encode_typed(param.data_type, value)?;

            entries.push(WriteEntry {
                addr,
                word_len,
                device_code,
                data: Bytes::from(data_bytes),
            });
        }

        // Execute typed write via session; any protocol error aborts the whole
        // action. Metrics are updated once per execute call.

        let series_max = self.inner.config.series.device_batch_in_word_points_max();
        let max_points = self
            .inner
            .config
            .max_points_per_batch
            .unwrap_or(series_max)
            .max(1);
        let max_bytes = self.inner.config.max_bytes_per_frame.unwrap_or(4096).max(1);

        let start_ts = Instant::now();
        if let Err(e) = McTypedApi::write_points_typed(
            &session,
            &PlannerConfig::new(max_points, max_bytes),
            entries,
        )
        .await
        {
            self.total_requests.fetch_add(1, Ordering::Relaxed);
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
            return Err(DriverError::ExecutionError(e.to_string()));
        }
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        let elapsed_ms = start_ts.elapsed().as_millis() as u64;
        self.successful_requests.fetch_add(1, Ordering::Relaxed);
        let prev = self.last_avg_response_time_ms.load(Ordering::Relaxed);
        let new_avg = if prev == 0 {
            elapsed_ms
        } else {
            (prev.saturating_mul(9) + elapsed_ms) / 10
        };
        self.last_avg_response_time_ms
            .store(new_avg, Ordering::Relaxed);

        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(serde_json::json!({
                "status": "ok",
                "deviceId": device.id,
                "action": action.name,
            })),
        })
    }

    async fn write_point(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let _device = device
            .downcast_ref::<McDevice>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimeDevice is not McDevice for McDriver".to_string(),
            ))?;
        let point = point
            .downcast_ref::<McPoint>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not McPoint for McDriver".to_string(),
            ))?;

        if !matches!(point.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            return Err(DriverError::ValidationError(
                "point is not writeable".to_string(),
            ));
        }

        let effective_timeout_ms = timeout_ms
            .unwrap_or(self.inner.connection_policy.write_timeout_ms)
            .max(1);
        let timeout_duration = tokio::time::Duration::from_millis(effective_timeout_ms);

        // Strict datatype guard (core should already validate, keep driver defensive).
        if !value.validate_datatype(point.data_type) {
            return Err(DriverError::ValidationError(format!(
                "type mismatch: expected {:?}, got {:?}",
                point.data_type,
                value.data_type()
            )));
        }

        let addr = McLogicalAddress::parse(&point.address.raw).map_err(|e| {
            DriverError::ConfigurationError(format!(
                "Invalid MC address '{}': {e}",
                point.address.raw
            ))
        })?;

        let (word_len, device_code) = if addr.device.is_bit() {
            if point.data_type != DataType::Boolean {
                return Err(DriverError::ConfigurationError(format!(
                    "MC bit device {:?} only supports Boolean data type for write_point",
                    addr.device
                )));
            }
            if addr.bit.is_some() {
                return Err(DriverError::ConfigurationError(format!(
                    "Bit-indexed MC address '{}' is not supported yet for write_point",
                    point.address.raw
                )));
            }
            let device_code =
                addr.device
                    .device_code_3e()
                    .ok_or(DriverError::ConfigurationError(format!(
                        "Unsupported MC bit device type for write_point: {:?}",
                        addr.device
                    )))?;
            (1, device_code)
        } else {
            if addr.bit.is_some() || (!addr.device.is_word() && !addr.device.is_dword()) {
                return Err(DriverError::ConfigurationError(format!(
                    "Unsupported MC address for write_point: '{}'",
                    point.address.raw
                )));
            }
            if addr.device.is_forbidden_batch_word_write() {
                return Err(DriverError::ConfigurationError(format!(
                    "MC device type {:?} is not allowed for batch word write (write_point)",
                    addr.device
                )));
            }
            let word_len = words_for_data_type(point.data_type, point.string_len_bytes)?;
            let device_code =
                addr.device
                    .device_code_3e()
                    .ok_or(DriverError::ConfigurationError(format!(
                        "Unsupported MC device type for write_point: {:?}",
                        addr.device
                    )))?;
            (word_len, device_code)
        };

        let dt = value.data_type();
        let data_bytes: Vec<u8> = (&value).try_into().map_err(|e: NGValueCastError| {
            DriverError::ValidationError(format!("Expected numeric value, got {:?}: {e}", dt))
        })?;

        // Acquire active session.
        let session = self
            .shared
            .session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)?;

        // Planner limits (same as execute_action)
        let series_max = self.inner.config.series.device_batch_in_word_points_max();
        let max_points = self
            .inner
            .config
            .max_points_per_batch
            .unwrap_or(series_max)
            .max(1);
        let max_bytes = self.inner.config.max_bytes_per_frame.unwrap_or(4096).max(1);

        let entries = vec![WriteEntry {
            addr,
            word_len,
            device_code,
            data: Bytes::from(data_bytes),
        }];
        let planner_cfg = PlannerConfig::new(max_points, max_bytes);

        let start_ts = Instant::now();
        let write_res = if timeout_ms.is_some() {
            match tokio::time::timeout(timeout_duration, {
                let session = Arc::clone(&session);
                async move { McTypedApi::write_points_typed(&session, &planner_cfg, entries).await }
            })
            .await
            {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(DriverError::ExecutionError(e.to_string())),
                Err(_elapsed) => Err(DriverError::Timeout(timeout_duration)),
            }
        } else {
            McTypedApi::write_points_typed(&session, &planner_cfg, entries)
                .await
                .map_err(|e| DriverError::ExecutionError(e.to_string()))
        };

        // Metrics
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        let elapsed_ms = start_ts.elapsed().as_millis() as u64;
        match &write_res {
            Ok(_) => {
                self.successful_requests.fetch_add(1, Ordering::Relaxed);
                let prev = self.last_avg_response_time_ms.load(Ordering::Relaxed);
                let new_avg = if prev == 0 {
                    elapsed_ms
                } else {
                    (prev.saturating_mul(9) + elapsed_ms) / 10
                };
                self.last_avg_response_time_ms
                    .store(new_avg, Ordering::Relaxed);
            }
            Err(_) => {
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
            }
        }

        write_res?;

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value),
        })
    }

    /// Subscribe to connection state updates.
    fn subscribe_connection_state(&self) -> watch::Receiver<SouthwardConnectionState> {
        self.conn_rx.clone()
    }

    /// Get driver health status based on connection and request counters.
    async fn health_check(&self) -> DriverResult<DriverHealth> {
        Ok(DriverHealth {
            status: if self.shared.healthy.load(Ordering::Acquire) {
                HealthStatus::Healthy
            } else {
                HealthStatus::Unhealthy
            },
            last_activity: Utc::now(),
            error_count: self.failed_requests.load(Ordering::Relaxed),
            success_rate: {
                let success = self.successful_requests.load(Ordering::Relaxed);
                let total = self.total_requests.load(Ordering::Relaxed).max(1);
                success as f64 / total as f64
            },
            average_response_time: StdDuration::from_millis(
                self.last_avg_response_time_ms.load(Ordering::Relaxed),
            ),
            details: None,
        })
    }
}
