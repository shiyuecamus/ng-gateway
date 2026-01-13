use crate::{
    codec::Dl645Codec,
    protocol::{
        error::ProtocolError,
        frame::{encode_address_from_str, Dl645Address, Dl645BaudRate, Dl645Body, Dl645TypedFrame},
        session::{
            BroadcastTimeSyncParams, ClearEventsParams, ClearMaxDemandParams, ClearMeterParams,
            Dl645Session, FreezeParams, ReadDataParams, UpdateBaudRateParams, WriteAddressParams,
            WriteDataParams,
        },
    },
    supervisor::{Dl645Supervisor, SessionEntry, SharedSession},
    types::{
        Dl645Action, Dl645Channel, Dl645Device, Dl645FunctionCode, Dl645Parameter, Dl645Point,
        Dl645Version,
    },
};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, AttributeData, DataPointType, Driver, DriverError,
    DriverHealth, DriverResult, ExecuteOutcome, ExecuteResult, HealthStatus, NGValue,
    NGValueCastError, NorthwardData, PointValue, RuntimeAction, RuntimeDevice, RuntimeParameter,
    RuntimePoint, SouthwardConnectionState, SouthwardInitContext, TelemetryData, WriteOutcome,
    WriteResult,
};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch, Mutex};
use tokio_util::sync::CancellationToken;

/// Production-ready DL/T 645 driver implementation skeleton.
///
/// This implementation focuses on providing a correct and observable structure
/// that integrates well with the gateway runtime. The protocol details and
/// optimized scheduling can be refined incrementally without changing the
/// public interface.
pub struct Dl645Driver {
    /// Runtime channel configuration.
    inner: Arc<Dl645Channel>,
    /// Shared session state managed by the supervisor.
    session: SharedSession,
    /// Deferred reconnect receiver, created during init and consumed once in `start`.
    reconnect_rx: Mutex<Option<mpsc::Receiver<()>>>,
    /// Driver-level cancellation token.
    cancel: CancellationToken,
    /// Connection state channel sender.
    conn_tx: watch::Sender<SouthwardConnectionState>,
    /// Connection state channel receiver.
    conn_rx: watch::Receiver<SouthwardConnectionState>,
    /// Started flag to prevent duplicate `start` calls.
    started: AtomicBool,
    /// Metrics: total requests issued by this driver.
    total_requests: AtomicU64,
    /// Metrics: successful requests.
    successful_requests: AtomicU64,
    /// Metrics: failed requests.
    failed_requests: AtomicU64,
    /// Metrics: exponential moving average of response time in milliseconds.
    last_avg_response_time_ms: AtomicU64,
}

impl Dl645Driver {
    /// Construct a driver instance from the initialization context.
    ///
    /// This method validates the runtime channel type, builds planner and
    /// session structures, and indexes points per device for fast lookup.
    pub fn with_context(ctx: SouthwardInitContext) -> DriverResult<Self> {
        let inner = ctx
            .runtime_channel
            .downcast_arc::<Dl645Channel>()
            .map_err(|_| {
                DriverError::ConfigurationError("Invalid Dl645Channel runtime type".to_string())
            })?;

        let (conn_tx, conn_rx) = watch::channel(SouthwardConnectionState::Disconnected);
        // Reconnect coordination channel between data-plane and supervisor.
        let (reconnect_tx, reconnect_rx) = mpsc::channel::<()>(1);
        let shared = Arc::new(SessionEntry::new_empty(
            reconnect_tx,
            inner.config.max_timeouts,
        ));
        let cancel = CancellationToken::new();

        Ok(Self {
            inner,
            session: shared,
            reconnect_rx: Mutex::new(Some(reconnect_rx)),
            cancel,
            conn_tx,
            conn_rx,
            started: AtomicBool::new(false),
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            last_avg_response_time_ms: AtomicU64::new(0),
        })
    }

    /// Helper to update response time metrics using an exponential moving average.
    fn update_response_metrics(&self, elapsed_ms: u64, success: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if success {
            self.successful_requests.fetch_add(1, Ordering::Relaxed);
            let prev = self.last_avg_response_time_ms.load(Ordering::Acquire);
            let new_avg = if prev == 0 {
                elapsed_ms
            } else {
                (prev.saturating_mul(9) + elapsed_ms) / 10
            };
            self.last_avg_response_time_ms
                .store(new_avg, Ordering::Release);
        } else {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Run a DL/T 645 operation against the current session with unified
    /// error handling, reconnect semantics and metrics.
    ///
    /// This helper mirrors the role of `run_modbus_op` in the Modbus driver:
    /// it wraps a single request/response exchange, maps protocol-level errors
    /// into the gateway's `DriverError` domain, updates reconnect counters and
    /// feeds latency information into the driver's metrics.
    async fn run_op<T, F, Fut>(
        &self,
        op_timeout: Duration,
        op_label: &'static str,
        op: F,
    ) -> DriverResult<T>
    where
        F: FnOnce(Arc<dyn Dl645Session>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, ProtocolError>> + Send + 'static,
        T: Send + 'static,
    {
        let session_shared = self.session.clone();

        let (res, elapsed_ms) = tokio::spawn(async move {
            let start = Instant::now();
            // Acquire current session snapshot (lock-free).
            let session_opt = session_shared.session.load_full();
            let handle = match session_opt {
                Some(h) => h,
                None => {
                    session_shared.healthy.store(false, Ordering::Release);
                    {
                        let mut last = session_shared.last_error.lock().await;
                        *last = Some("no active DL/T 645 session".to_string());
                    }
                    tracing::warn!(
                        op = op_label,
                        "DL/T 645 session not available, request reconnect"
                    );
                    let _ = session_shared.reconnect_tx.try_send(());
                    return (Err(DriverError::ServiceUnavailable), 0);
                }
            };

            let sess = Arc::clone(&handle.0);
            // Wrap operation in timeout
            let op_res = match tokio::time::timeout(op_timeout, op(sess)).await {
                Ok(res) => res,
                Err(_) => Err(ProtocolError::Timeout(op_timeout)),
            };

            let res = match op_res {
                Ok(v) => {
                    session_shared
                        .consecutive_timeouts
                        .store(0, Ordering::Release);
                    Ok(v)
                }
                Err(proto_err) => {
                    match &proto_err {
                        ProtocolError::Timeout(_) => {
                            tracing::warn!(op = op_label, "DL/T 645 operation timeout");
                            let new_count = session_shared
                                .consecutive_timeouts
                                .fetch_add(1, Ordering::Relaxed)
                                .saturating_add(1);
                            let threshold = session_shared.timeout_reconnect_threshold;
                            if threshold > 0 && new_count >= threshold as u64 {
                                session_shared.healthy.store(false, Ordering::Release);
                                {
                                    let mut last = session_shared.last_error.lock().await;
                                    *last = Some(format!(
                                        "DL/T 645 consecutive timeouts reached threshold: {}",
                                        new_count
                                    ));
                                }
                                tracing::warn!(
                                    op = op_label,
                                    timeout_count = new_count,
                                    threshold,
                                    "DL/T 645 timeout threshold reached, requesting reconnect"
                                );
                                let _ = session_shared.reconnect_tx.try_send(());
                                session_shared
                                    .consecutive_timeouts
                                    .store(0, Ordering::Release);
                            }
                        }
                        ProtocolError::Transport(msg) => {
                            session_shared.healthy.store(false, Ordering::Release);
                            {
                                let mut last = session_shared.last_error.lock().await;
                                *last = Some(msg.clone());
                            }
                            tracing::warn!(
                                op = op_label,
                                error = %msg,
                                "DL/T 645 transport error, requesting reconnect"
                            );
                            let _ = session_shared.reconnect_tx.try_send(());
                        }
                        ProtocolError::Io(e) => {
                            session_shared.healthy.store(false, Ordering::Release);
                            {
                                let mut last = session_shared.last_error.lock().await;
                                *last = Some(e.to_string());
                            }
                            tracing::warn!(
                                op = op_label,
                                error = %e,
                                "DL/T 645 IO error, requesting reconnect"
                            );
                            let _ = session_shared.reconnect_tx.try_send(());
                        }
                        _ => {}
                    }

                    Err(proto_err.into())
                }
            };

            let elapsed = start.elapsed().as_millis() as u64;
            (res, elapsed)
        })
        .await
        .map_err(|e| DriverError::ExecutionError(e.to_string()))?;

        self.update_response_metrics(elapsed_ms, res.is_ok());
        res
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    async fn handle_write_data(
        &self,
        security_params: (u32, Option<u32>),
        param: &Dl645Parameter,
        value: &NGValue,
        address: Dl645Address,
        version: Dl645Version,
        timeout: Duration,
    ) -> DriverResult<()> {
        let (password, operator_code) = security_params;
        let di_u32 = param.di.ok_or(DriverError::ConfigurationError(
            "DL/T 645 write data parameter missing DI".to_string(),
        ))?;

        let value_bytes = Dl645Codec::encode_parameter_value(param, value)?;

        self.run_op::<(), _, _>(timeout, "write_data", move |session| {
            let vb = value_bytes;
            async move {
                session
                    .write_data(
                        WriteDataParams {
                            version,
                            address,
                            di: di_u32,
                            value_bytes: vb,
                            password,
                            operator_code,
                        },
                        timeout,
                        1,
                    )
                    .await
                    .map(|_| ())
            }
        })
        .await
    }

    #[inline]
    async fn handle_write_address(
        &self,
        value: &NGValue,
        address: Dl645Address,
    ) -> DriverResult<()> {
        let new_addr_str = match value {
            NGValue::String(s) => s.as_ref(),
            _ => {
                return Err(DriverError::ConfigurationError(
                    "DL/T 645 write address expects string value".to_string(),
                ))
            }
        };

        if new_addr_str.len() != 12 || !new_addr_str.chars().all(|c| c.is_ascii_digit()) {
            return Err(DriverError::ConfigurationError(
                "DL/T 645 write address value must be a 12-digit decimal string".to_string(),
            ));
        }

        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let new_addr = encode_address_from_str(new_addr_str)?;
        let version = self.inner.config.version;

        self.run_op::<(), _, _>(timeout, "write_address", move |session| async move {
            session
                .write_address(
                    WriteAddressParams::from_new_address(version, address, new_addr)?,
                    timeout,
                )
                .await
                .map(|_| ())
        })
        .await
    }

    #[inline]
    async fn handle_broadcast_time_sync(&self, value: &NGValue) -> DriverResult<()> {
        let ts_secs: i64 = value.try_into().map_err(|e: NGValueCastError| {
            DriverError::ConfigurationError(format!(
                "DL/T 645 time sync expects Unix timestamp seconds: {e}"
            ))
        })?;

        if ts_secs < 0 {
            return Err(DriverError::ConfigurationError(
                "DL/T 645 time sync value must be non-negative Unix timestamp".to_string(),
            ))?;
        }

        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let ts = ts_secs;
        let version = self.inner.config.version;

        self.run_op::<(), _, _>(timeout, "broadcast_time_sync", move |session| async move {
            session
                .broadcast_time_sync(
                    BroadcastTimeSyncParams::from_unix_secs(version, ts)?,
                    timeout,
                )
                .await
        })
        .await
    }

    #[inline]
    async fn handle_freeze(&self, value: &NGValue, address: Dl645Address) -> DriverResult<()> {
        if matches!(self.inner.config.version, Dl645Version::V1997) {
            return Err(DriverError::ConfigurationError(
                "DL/T 645 freeze is not supported for version V1997".to_string(),
            ));
        }
        let freeze_str = match value {
            NGValue::String(s) => s.as_ref(),
            _ => {
                return Err(DriverError::ConfigurationError(
                    "DL/T 645 freeze expects string value".to_string(),
                ))
            }
        };

        if freeze_str.len() != 8 || !freeze_str.chars().all(|c| c.is_ascii_digit()) {
            return Err(DriverError::ConfigurationError(
                "DL/T 645 freeze value must be 8 decimal digits (MMDDhhmm)".to_string(),
            ));
        }

        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let freeze_pattern = freeze_str.to_string();
        let version = self.inner.config.version;

        self.run_op::<(), _, _>(timeout, "freeze", move |session| {
            let pattern = freeze_pattern;
            async move {
                session
                    .freeze(
                        FreezeParams::from_pattern_str(version, address, &pattern)?,
                        timeout,
                    )
                    .await
                    .map(|_| ())
            }
        })
        .await
    }

    #[inline]
    async fn handle_update_baud_rate(
        &self,
        value: &NGValue,
        address: Dl645Address,
    ) -> DriverResult<()> {
        let baud: u64 = value.try_into().map_err(|e: NGValueCastError| {
            DriverError::ConfigurationError(format!(
                "DL/T 645 update baud rate expects number value: {e}"
            ))
        })?;

        let plain_code: u8 = Dl645BaudRate::try_from(baud as u32)
            .map(|rate| rate.as_code(self.inner.config.version))
            .map_err(|_| {
                DriverError::ConfigurationError(format!("Invalid DL/T 645 baud rate: {baud}"))
            })?;

        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let version = self.inner.config.version;

        self.run_op::<(), _, _>(timeout, "update_baud_rate", move |session| async move {
            session
                .update_baud_rate(
                    UpdateBaudRateParams {
                        version,
                        address,
                        code: plain_code,
                    },
                    timeout,
                )
                .await
                .map(|_| ())
        })
        .await
    }

    #[inline]
    async fn handle_clear_max_demand(
        &self,
        security_params: (u32, Option<u32>),
        address: Dl645Address,
    ) -> DriverResult<()> {
        let (password, operator_code) = security_params;
        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let version = self.inner.config.version;

        self.run_op::<(), _, _>(timeout, "clear_max_demand", move |session| async move {
            session
                .clear_max_demand(
                    ClearMaxDemandParams {
                        version,
                        address,
                        password,
                        operator_code,
                    },
                    timeout,
                )
                .await
                .map(|_| ())
        })
        .await
    }

    #[inline]
    async fn handle_clear_meter(
        &self,
        security_params: (u32, Option<u32>),
        address: Dl645Address,
    ) -> DriverResult<()> {
        if matches!(self.inner.config.version, Dl645Version::V1997) {
            return Err(DriverError::ConfigurationError(
                "DL/T 645 clear meter is not supported for version V1997".to_string(),
            ));
        }
        let (password, operator_code) = security_params;
        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let version = self.inner.config.version;

        self.run_op::<(), _, _>(timeout, "clear_meter", move |session| async move {
            session
                .clear_meter(
                    ClearMeterParams {
                        version,
                        address,
                        password,
                        operator_code,
                    },
                    timeout,
                )
                .await
                .map(|_| ())
        })
        .await
    }

    #[inline]
    async fn handle_clear_events(
        &self,
        security_params: (u32, Option<u32>),
        value: &NGValue,
        address: Dl645Address,
    ) -> DriverResult<()> {
        if matches!(self.inner.config.version, Dl645Version::V1997) {
            return Err(DriverError::ConfigurationError(
                "DL/T 645 clear events is not supported for version V1997".to_string(),
            ));
        }

        let (password, operator_code) = security_params;
        let di_str = match value {
            NGValue::String(s) => s.as_ref(),
            _ => {
                return Err(DriverError::ConfigurationError(
                    "DL/T 645 clear events expects hex string value".to_string(),
                ))
            }
        };

        if di_str.len() != 8 || !di_str.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(DriverError::ConfigurationError(
                "DL/T 645 clear events value must be 8 hex characters".to_string(),
            ))?;
        }

        let di_u32 = u32::from_str_radix(di_str, 16).map_err(|_| {
            DriverError::ConfigurationError(
                "DL/T 645 clear events value must be valid 32-bit hex".to_string(),
            )
        })?;

        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let version = self.inner.config.version;

        self.run_op::<(), _, _>(timeout, "clear_events", move |session| async move {
            session
                .clear_events(
                    ClearEventsParams {
                        version,
                        address,
                        di: di_u32,
                        password,
                        operator_code,
                    },
                    timeout,
                )
                .await
                .map(|_| ())
        })
        .await
    }
}

#[async_trait]
impl Driver for Dl645Driver {
    /// Start the DL/T 645 driver.
    ///
    /// This spawns the supervisor loop that manages the underlying serial
    /// session. It is safe to call multiple times; subsequent calls are no-ops.
    async fn start(&self) -> DriverResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        // Spawn connection supervisor.
        let channel = Arc::clone(&self.inner);
        let shared = Arc::clone(&self.session);
        let cancel = self.cancel.child_token();
        let mut rx_guard = self.reconnect_rx.lock().await;
        let reconnect_rx = rx_guard.take().ok_or(DriverError::ExecutionError(
            "reconnect receiver already consumed".to_string(),
        ))?;
        let supervisor = Dl645Supervisor::new(shared, cancel, self.conn_tx.clone(), reconnect_rx);
        supervisor.run(channel).await?;

        Ok(())
    }

    /// Stop the driver and release resources.
    async fn stop(&self) -> DriverResult<()> {
        self.cancel.cancel();
        self.session.shutdown.store(true, Ordering::Release);
        Ok(())
    }

    async fn collect_data(
        &self,
        device: Arc<dyn RuntimeDevice>,
        data_points: Arc<[Arc<dyn RuntimePoint>]>,
    ) -> DriverResult<Vec<NorthwardData>> {
        let d = device
            .downcast_ref::<Dl645Device>()
            .ok_or(DriverError::InvalidEntity(
                "Device is not a DL/T 645 device in this driver".to_string(),
            ))?;

        let version = self.inner.config.version;
        let device_id = d.id;
        let device_name = d.device_name.clone();

        let concrete_points = data_points
            .iter()
            .filter_map(|p| Arc::clone(p).downcast_arc::<Dl645Point>().ok())
            .collect::<Vec<_>>();
        if concrete_points.is_empty() {
            return Ok(Vec::new());
        }

        let mut result = Vec::with_capacity(concrete_points.len());

        for point in concrete_points.iter() {
            // Keep local copies minimal on hot paths; use `point.id` + typed value for routing.
            let timeout_ms = self.inner.connection_policy.read_timeout_ms;
            let timeout = Duration::from_millis(timeout_ms.max(1));
            let di = point.di;
            let address = d.address;

            let op_res = self
                .run_op(timeout, "read_data", move |session| async move {
                    let logical = session
                        .read_data(
                            ReadDataParams {
                                version,
                                address,
                                di,
                            },
                            timeout,
                            8,
                        )
                        .await?;

                    match logical.frames.last() {
                        Some(last) => Ok(Dl645TypedFrame {
                            address: logical.address,
                            control: last.control,
                            body: Dl645Body::Raw(Bytes::from(logical.payload)),
                        }),
                        None => Err(ProtocolError::Semantic(
                            "DL/T 645 logical response contained no frames".to_string(),
                        )),
                    }
                })
                .await;

            match op_res {
                Ok(resp) => {
                    let value = match Dl645Codec::decode_point_value(version, point, &resp) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                device_id,
                                point = %point.key,
                                error = %e,
                                "DL/T 645 decode value error"
                            );
                            continue;
                        }
                    };

                    match point.r#type {
                        DataPointType::Telemetry => {
                            let data = TelemetryData::new(
                                device_id,
                                device_name.clone(),
                                vec![PointValue {
                                    point_id: point.id,
                                    point_key: Arc::<str>::from(point.key.as_str()),
                                    value,
                                }],
                            );
                            result.push(NorthwardData::Telemetry(data));
                        }
                        DataPointType::Attribute => {
                            let data = AttributeData::new_client_attributes(
                                device_id,
                                device_name.clone(),
                                vec![PointValue {
                                    point_id: point.id,
                                    point_key: Arc::<str>::from(point.key.as_str()),
                                    value,
                                }],
                            );
                            result.push(NorthwardData::Attributes(data));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        device_id,
                        point = %point.key,
                        error = %e,
                        "DL/T 645 request error"
                    );
                }
            }
        }

        Ok(result)
    }

    /// Execute a DL/T 645 action on a device.
    ///
    /// The implementation resolves typed parameters, then iterates over each
    /// parameter and dispatches to the appropriate protocol command based on
    /// its `function_code` (for example "write data" versus "broadcast time
    /// synchronization"). Currently only the classic "write data" command is
    /// implemented; other function codes will result in a configuration error
    /// until their protocol handlers are added.
    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let d = device
            .downcast_ref::<Dl645Device>()
            .ok_or(DriverError::InvalidEntity(
                "Device is not a DL/T 645 device in this driver".to_string(),
            ))?;
        let action = action
            .downcast_ref::<Dl645Action>()
            .ok_or(DriverError::InvalidEntity(
                "Action is not a DL/T 645 action in this driver".to_string(),
            ))?;

        let resolved = downcast_parameters::<Dl645Parameter>(parameters)?;

        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let address = d.address;
        let version = self.inner.config.version;

        for (param, value) in resolved.iter() {
            let result: DriverResult<()> = match param.function_code {
                Dl645FunctionCode::WriteData => {
                    self.handle_write_data(
                        d.security_params()?,
                        param,
                        value,
                        address,
                        version,
                        timeout,
                    )
                    .await
                }
                Dl645FunctionCode::WriteAddress => self.handle_write_address(value, address).await,
                Dl645FunctionCode::BroadcastTimeSync => {
                    self.handle_broadcast_time_sync(value).await
                }
                Dl645FunctionCode::Freeze => self.handle_freeze(value, address).await,
                Dl645FunctionCode::UpdateBaudRate => {
                    self.handle_update_baud_rate(value, address).await
                }
                Dl645FunctionCode::ModifyPassword => Err(DriverError::ConfigurationError(
                    "DL/T 645 modify password is not supported".to_string(),
                )),
                Dl645FunctionCode::ClearMaxDemand => {
                    self.handle_clear_max_demand(d.security_params()?, address)
                        .await
                }
                Dl645FunctionCode::ClearMeter => {
                    self.handle_clear_meter(d.security_params()?, address).await
                }
                Dl645FunctionCode::ClearEvents => {
                    self.handle_clear_events(d.security_params()?, value, address)
                        .await
                }
                other => Err(DriverError::ConfigurationError(format!(
                    "Unsupported function in execute action phase: {:?}",
                    other
                ))),
            };

            result?;
        }

        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(json!(format!("Action '{}' executed", action.name()))),
        })
    }

    async fn write_point(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let device = device
            .downcast_ref::<Dl645Device>()
            .ok_or(DriverError::InvalidEntity(
                "Device is not a DL/T 645 device in this driver".to_string(),
            ))?;
        let point = point
            .downcast_ref::<Dl645Point>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not Dl645Point for Dl645Driver".to_string(),
            ))?;

        if !matches!(point.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            return Err(DriverError::ValidationError(
                "point is not writeable".to_string(),
            ));
        }

        let effective_timeout_ms = timeout_ms
            .unwrap_or(self.inner.connection_policy.write_timeout_ms)
            .max(1);
        let timeout = Duration::from_millis(effective_timeout_ms);
        let version = self.inner.config.version;
        let address = device.address;

        // Per design: write_point is fixed to WriteData(DI).
        let param = Dl645Parameter {
            name: point.name.clone(),
            key: point.key.clone(),
            data_type: point.data_type,
            required: true,
            default_value: None,
            max_value: point.max_value,
            min_value: point.min_value,
            decimals: point.decimals,
            function_code: Dl645FunctionCode::WriteData,
            di: Some(point.di),
        };

        // Strict datatype guard (core should already validate, keep driver defensive).
        if !value.validate_datatype(point.data_type) {
            return Err(DriverError::ValidationError(format!(
                "type mismatch: expected {:?}, got {:?}",
                point.data_type,
                value.data_type()
            )));
        }

        self.handle_write_data(
            device.security_params()?,
            &param,
            &value,
            address,
            version,
            timeout,
        )
        .await?;

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value),
        })
    }

    /// Subscribe to DL/T 645 channel connection state updates.
    fn subscribe_connection_state(&self) -> watch::Receiver<SouthwardConnectionState> {
        self.conn_rx.clone()
    }

    /// Provide aggregated driver health information.
    async fn health_check(&self) -> DriverResult<DriverHealth> {
        let total = self.total_requests.load(Ordering::Relaxed);
        let success = self.successful_requests.load(Ordering::Relaxed);
        let error_count = self.failed_requests.load(Ordering::Relaxed);
        let success_rate = if total == 0 {
            1.0
        } else {
            success as f64 / total as f64
        };

        let status = if error_count == 0 {
            HealthStatus::Healthy
        } else if success_rate > 0.8 {
            HealthStatus::Degraded
        } else {
            HealthStatus::Unhealthy
        };

        Ok(DriverHealth {
            status,
            last_activity: Utc::now(),
            error_count,
            success_rate,
            average_response_time: Duration::from_millis(
                self.last_avg_response_time_ms.load(Ordering::Relaxed),
            ),
            details: None,
        })
    }
}
