use super::{
    codec::S7Codec,
    protocol::frame::S7TransportSize,
    supervisor::{S7Supervisor, SessionEntry, SharedSession},
    types::{S7Action, S7Device, S7Parameter, S7Point},
};
use crate::types::S7Channel;
use async_trait::async_trait;
use chrono::Utc;
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, AttributeData, DataPointType, Driver, DriverError,
    DriverHealth, DriverResult, ExecuteOutcome, ExecuteResult, HealthStatus, NGValue,
    NorthwardData, PointValue, RuntimeAction, RuntimeDevice, RuntimeParameter, RuntimePoint,
    SouthwardConnectionState, SouthwardInitContext, TelemetryData, WriteOutcome, WriteResult,
};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration as StdDuration, Instant};
use tokio::{sync::watch, time::timeout};
use tokio_util::sync::CancellationToken;
use tracing::instrument;

/// S7 driver state and metrics
///
/// This struct keeps minimal state required by the gateway driver trait. The
/// detailed protocol actors will be introduced later.
pub struct S7Driver {
    /// Raw driver configuration (typed in factory)
    inner: Arc<S7Channel>,
    /// Shared session (owned by driver like s7)
    shared: SharedSession,
    /// Started flag to guard duplicate start()
    started: AtomicBool,
    /// Cancel token
    cancel_token: CancellationToken,
    /// Moving counters for quick health
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    last_avg_response_time_ms: AtomicU64,
    /// Connection state broadcast channel
    conn_tx: watch::Sender<SouthwardConnectionState>,
    conn_rx: watch::Receiver<SouthwardConnectionState>,
}

impl S7Driver {
    pub fn with_context(ctx: SouthwardInitContext) -> DriverResult<Self> {
        let (tx, rx) = watch::channel(SouthwardConnectionState::Disconnected);

        let inner = ctx
            .runtime_channel
            .downcast_arc::<S7Channel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid S7Channel".to_string()))?;

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
}

#[async_trait]
impl Driver for S7Driver {
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
        let supervisor = S7Supervisor::new(shared, cancel, self.conn_tx.clone());
        supervisor.run(Arc::clone(&self.inner)).await?;
        Ok(())
    }

    #[inline]
    #[instrument(level = "info", skip_all)]
    async fn stop(&self) -> DriverResult<()> {
        // Cancel handle_asdu tasks and reconnect attempts
        self.cancel_token.cancel();
        self.started.store(false, Ordering::Release);
        Ok(())
    }

    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn collect_data(
        &self,
        device: Arc<dyn RuntimeDevice>,
        data_points: Arc<[Arc<dyn RuntimePoint>]>,
    ) -> DriverResult<Vec<NorthwardData>> {
        if let Some(md) = device.downcast_ref::<S7Device>() {
            // Downcast points to S7Point and collect readable
            let s7_points = data_points
                .iter()
                .filter_map(|p| Arc::clone(p).downcast_arc::<S7Point>().ok())
                .filter(|mp| matches!(mp.access_mode(), AccessMode::Read | AccessMode::ReadWrite))
                .collect::<Vec<_>>();
            if s7_points.is_empty() {
                return Ok(Vec::new());
            }

            // Acquire active session
            let session = self
                .shared
                .session
                .load_full()
                .ok_or(DriverError::ServiceUnavailable)?;

            // Batch read
            let addresses = s7_points.iter().map(|p| &p.address).collect::<Vec<_>>();
            let start_ts = Instant::now();
            let results = match session.read_addresses_typed(&addresses).await {
                Ok(r) => {
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
                    r
                }
                Err(e) => {
                    self.total_requests.fetch_add(1, Ordering::Relaxed);
                    self.failed_requests.fetch_add(1, Ordering::Relaxed);
                    return Err(DriverError::ExecutionError(e.to_string()));
                }
            };

            let mut telemetry_values: Vec<PointValue> = Vec::with_capacity(s7_points.len());
            let mut attribute_values: Vec<PointValue> = Vec::with_capacity(s7_points.len());

            for (p, it) in s7_points.iter().zip(results.into_iter()) {
                if it.value.is_none() {
                    continue;
                }
                let v = it.value.unwrap();
                if let Some(value) = S7Codec::to_value(&v, p.data_type(), p.scale()) {
                    match p.r#type() {
                        DataPointType::Telemetry => {
                            telemetry_values.push(PointValue {
                                point_id: p.id,
                                point_key: Arc::<str>::from(p.key.as_str()),
                                value,
                            });
                        }
                        DataPointType::Attribute => {
                            attribute_values.push(PointValue {
                                point_id: p.id,
                                point_key: Arc::<str>::from(p.key.as_str()),
                                value,
                            });
                        }
                    }
                }
            }

            let mut out: Vec<NorthwardData> = Vec::with_capacity(2);
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

            return Ok(out);
        }
        Err(DriverError::ConfigurationError(
            "RuntimeDevice is not S7Device for S7Driver".to_string(),
        ))
    }

    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let action = action
            .downcast_ref::<S7Action>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimeAction is not S7Action".to_string(),
            ))?;
        let device = device
            .downcast_ref::<S7Device>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimeDevice is not S7Device".to_string(),
            ))?;

        let resolved = downcast_parameters::<S7Parameter>(parameters)?;

        // Acquire active session
        let session = self
            .shared
            .session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)?;

        // Build write items without applying scale (per spec)
        let mut items = Vec::with_capacity(resolved.len());
        for (param, value) in resolved.iter() {
            let ts = S7TransportSize::try_from(param.address.transport_size).map_err(|_| {
                DriverError::ConfigurationError("Invalid transport size".to_string())
            })?;
            let val = S7Codec::from_value(value, ts)?;
            items.push((&param.address, val));
        }

        // Execute with metrics
        let start_ts = Instant::now();
        match session.write_addresses_typed(&items).await {
            Ok(_) => {
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
            }
            Err(e) => {
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
                return Err(DriverError::ExecutionError(e.to_string()));
            }
        }
        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(serde_json::json!(format!(
                "Action '{}' executed on device {}",
                action.name(),
                device.device_name
            ))),
        })
    }

    async fn write_point(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let point = point
            .downcast_ref::<S7Point>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not S7Point for S7Driver".to_string(),
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

        // Acquire active session
        let session = self
            .shared
            .session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)?;

        let ts = S7TransportSize::try_from(point.address.transport_size).map_err(|_| {
            DriverError::ConfigurationError("Invalid transport size in S7 address".to_string())
        })?;
        let s7_value = S7Codec::from_value(&value, ts)?;
        // `timeout()` runs on the driver runtime via spawn, so the future must be `'static`.
        // Avoid borrowing `point`/`session` across await by moving owned data into `async move`.
        let address = point.address.clone();

        let start_ts = Instant::now();
        let write_res = if timeout_ms.is_some() {
            match timeout(timeout_duration, {
                let session = Arc::clone(&session);
                async move { session.write_address_typed(&address, s7_value).await }
            })
            .await
            {
                Ok(Ok(_ack)) => Ok(()),
                Ok(Err(e)) => Err(DriverError::ExecutionError(e.to_string())),
                Err(_elapsed) => Err(DriverError::Timeout(timeout_duration)),
            }
        } else {
            session
                .write_address_typed(&address, s7_value)
                .await
                .map(|_ack| ())
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

    #[inline]
    fn subscribe_connection_state(&self) -> watch::Receiver<SouthwardConnectionState> {
        self.conn_rx.clone()
    }

    #[inline]
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
