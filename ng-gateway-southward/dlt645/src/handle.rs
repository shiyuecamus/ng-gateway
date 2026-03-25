//! DL/T 645 southward data-plane handle.
//!
//! This is the **only** hot-path object published by the SDK supervision loop.
//! It MUST be cheap to clone (`Arc`), safe to use concurrently, and avoid extra allocations.
//!
//! Design:
//! - The transport is established by `Dl645Connector`.
//! - A concrete protocol session (`dyn protocol::session::Dl645Session`) is attached/detached by
//!   `Dl645Session` (supervision layer).
//! - Data-plane methods fail fast with `ServiceUnavailable` when no active session exists.

use super::{
    codec::Dl645Codec,
    protocol::{
        error::ProtocolError,
        frame::{encode_address_from_str, Dl645Address, Dl645BaudRate, Dl645Body, Dl645TypedFrame},
        session::{
            BroadcastTimeSyncParams, ClearEventsParams, ClearMaxDemandParams, ClearMeterParams,
            Dl645Session as ProtoSession, FreezeParams, ReadDataParams, UpdateBaudRateParams,
            WriteAddressParams, WriteDataParams,
        },
    },
    types::{
        Dl645Action, Dl645Channel, Dl645Device, Dl645FunctionCode, Dl645Parameter, Dl645Point,
        Dl645Version,
    },
};
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use bytes::Bytes;
use ng_gateway_sdk::{
    downcast_parameters, supervision::ReconnectHandle, AccessMode, AttributeData, CollectItem,
    DataPointType, DriverError, DriverResult, ExecuteOutcome, ExecuteResult, NGValue,
    NGValueCastError, NorthwardData, PointValue, RuntimeAction, RuntimeDelta, RuntimeDevice,
    RuntimeParameter, RuntimePoint, SouthwardHandle, TelemetryData, WriteOutcome, WriteResult,
};
use serde_json::json;
use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

/// Concrete session wrapper needed for `ArcSwapOption` (trait objects are unsized).
pub struct SessionHandle(pub Arc<dyn ProtoSession>);

impl From<ProtocolError> for DriverError {
    /// Map protocol-level errors into the gateway's `DriverError` domain.
    ///
    /// - Codec/structural issues are treated as `CodecError`.
    /// - Semantic and device-level exceptions are mapped to `ExecutionError`.
    /// - Timeouts are mapped to `Timeout`, preserving the duration.
    /// - Transport/IO failures are mapped to `SessionError` so that the supervisor
    ///   can treat them as fatal for the underlying link.
    fn from(err: ProtocolError) -> Self {
        match err {
            ProtocolError::InvalidFrame(_)
            | ProtocolError::ChecksumMismatch
            | ProtocolError::InvalidControl(_)
            | ProtocolError::FrameTooLarge(_) => DriverError::CodecError(err.to_string()),
            ProtocolError::Exception(_) | ProtocolError::Semantic(_) => {
                DriverError::ExecutionError(err.to_string())
            }
            ProtocolError::Timeout(d) => DriverError::Timeout(d),
            ProtocolError::Transport(msg) => DriverError::SessionError(msg),
            ProtocolError::Io(e) => DriverError::SessionError(e.to_string()),
        }
    }
}

/// DL/T 645 data-plane handle.
pub struct Dl645Handle {
    /// Runtime channel configuration.
    inner: Arc<Dl645Channel>,
    /// Current active protocol session (lock-free access).
    session: ArcSwapOption<SessionHandle>,
    /// Timeout-based reconnect threshold.
    timeout_reconnect_threshold: u32,
    /// Consecutive timeouts observed on the data-plane.
    consecutive_timeouts: AtomicU64,
    /// Best-effort reconnect request handle (injected by `Dl645Session::init`).
    reconnect: OnceLock<ReconnectHandle>,
}

impl Dl645Handle {
    /// Create a new handle for the given channel (no I/O).
    #[inline]
    pub fn new(inner: Arc<Dl645Channel>) -> Self {
        Self {
            timeout_reconnect_threshold: inner.config.max_timeouts,
            inner,
            session: ArcSwapOption::from(None),
            consecutive_timeouts: AtomicU64::new(0),
            reconnect: OnceLock::new(),
        }
    }

    /// Attach protocol session for this attempt.
    #[inline]
    pub(crate) fn attach_session(&self, session: Arc<dyn ProtoSession>) {
        self.session.store(Some(Arc::new(SessionHandle(session))));
        self.consecutive_timeouts.store(0, Ordering::Release);
    }

    /// Detach protocol session (best-effort).
    #[inline]
    pub(crate) fn detach_session(&self) {
        self.session.store(None);
        self.consecutive_timeouts.store(0, Ordering::Release);
    }

    /// Inject reconnect handle (best-effort, idempotent).
    #[inline]
    pub(crate) fn set_reconnect(&self, reconnect: ng_gateway_sdk::supervision::ReconnectHandle) {
        let _ = self.reconnect.set(reconnect);
    }

    #[inline]
    fn try_request_reconnect(&self, reason: &'static str) {
        if let Some(h) = self.reconnect.get() {
            let _ = h.try_request_reconnect(reason);
        }
    }

    #[inline]
    fn load_session(&self) -> DriverResult<Arc<dyn ProtoSession>> {
        self.session
            .load_full()
            .map(|h| Arc::clone(&h.0))
            .ok_or(DriverError::ServiceUnavailable)
    }

    /// Apply protocol error policy:
    /// - timeout counter + threshold-based reconnect
    /// - transport/io reconnect
    #[inline]
    fn on_proto_error(&self, err: &ProtocolError) {
        match err {
            ProtocolError::Timeout(_) => {
                let new_count = self
                    .consecutive_timeouts
                    .fetch_add(1, Ordering::Relaxed)
                    .saturating_add(1);
                let threshold = self.timeout_reconnect_threshold;
                if threshold > 0 && new_count >= threshold as u64 {
                    self.try_request_reconnect("dlt645 timeout threshold reached");
                    self.consecutive_timeouts.store(0, Ordering::Release);
                }
            }
            ProtocolError::Transport(_) | ProtocolError::Io(_) => {
                self.try_request_reconnect("dlt645 transport/io error");
            }
            _ => {}
        }
    }

    /// Run a DL/T 645 operation against the current session with unified
    /// error handling, reconnect semantics and metrics.
    async fn run_op<T, F, Fut>(
        &self,
        op_timeout: Duration,
        _op_label: &'static str,
        op: F,
    ) -> DriverResult<T>
    where
        F: FnOnce(Arc<dyn ProtoSession>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, ProtocolError>> + Send + 'static,
        T: Send + 'static,
    {
        let sess = match self.load_session() {
            Ok(s) => s,
            Err(e) => {
                self.try_request_reconnect("dlt645 no active session");
                return Err(e);
            }
        };

        match tokio::time::timeout(op_timeout, op(sess)).await {
            Ok(Ok(v)) => {
                self.consecutive_timeouts.store(0, Ordering::Release);
                Ok(v)
            }
            Ok(Err(proto_err)) => {
                self.on_proto_error(&proto_err);
                Err(proto_err.into())
            }
            Err(_) => {
                let proto_err = ProtocolError::Timeout(op_timeout);
                self.on_proto_error(&proto_err);
                Err(proto_err.into())
            }
        }
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
                .map(|_| ())
        })
        .await
    }

    #[inline]
    async fn handle_update_baud_rate(
        &self,
        value: &NGValue,
        address: Dl645Address,
        version: Dl645Version,
    ) -> DriverResult<()> {
        let baud: u64 = value.try_into().map_err(|e: NGValueCastError| {
            DriverError::ConfigurationError(format!(
                "DL/T 645 update baud rate expects number value: {e}"
            ))
        })?;

        let plain_code: u8 = Dl645BaudRate::try_from(baud as u32)
            .map(|rate| rate.as_code(version))
            .map_err(|_| {
                DriverError::ConfigurationError(format!("Invalid DL/T 645 baud rate: {baud}"))
            })?;

        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);

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
    async fn handle_freeze(
        &self,
        value: &NGValue,
        address: Dl645Address,
        version: Dl645Version,
    ) -> DriverResult<()> {
        if matches!(version, Dl645Version::V1997) {
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
    async fn handle_clear_max_demand(
        &self,
        security_params: (u32, Option<u32>),
        address: Dl645Address,
        version: Dl645Version,
    ) -> DriverResult<()> {
        let (password, operator_code) = security_params;
        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);

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
        version: Dl645Version,
    ) -> DriverResult<()> {
        let (password, operator_code) = security_params;
        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);

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
        value: &NGValue,
        security_params: (u32, Option<u32>),
        address: Dl645Address,
        version: Dl645Version,
    ) -> DriverResult<()> {
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
            ));
        }

        let di_u32 = u32::from_str_radix(di_str, 16).map_err(|_| {
            DriverError::ConfigurationError(
                "DL/T 645 clear events value must be valid 32-bit hex".to_string(),
            )
        })?;

        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let timeout = Duration::from_millis(timeout_ms);

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
impl SouthwardHandle for Dl645Handle {
    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        let (device, data_points) = items.first().ok_or(DriverError::ValidationError(
            "collect_data called with empty items".to_string(),
        ))?;
        if items.len() != 1 {
            return Err(DriverError::ConfigurationError(
                "DL/T 645 driver does not support grouped collection".to_string(),
            ));
        }

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
                                    ts: None,
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
                                    ts: None,
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
        let version = self.inner.config.version;
        let address = d.address;

        let security_params = d.security_params()?;

        for (param, value) in resolved.iter() {
            match param.function_code {
                Dl645FunctionCode::WriteData => {
                    self.handle_write_data(
                        security_params,
                        param,
                        value,
                        address,
                        version,
                        timeout,
                    )
                    .await?;
                }
                Dl645FunctionCode::WriteAddress => {
                    self.handle_write_address(value, address).await?;
                }
                Dl645FunctionCode::BroadcastTimeSync => {
                    self.handle_broadcast_time_sync(value).await?;
                }
                Dl645FunctionCode::UpdateBaudRate => {
                    self.handle_update_baud_rate(value, address, version)
                        .await?;
                }
                Dl645FunctionCode::Freeze => {
                    self.handle_freeze(value, address, version).await?;
                }
                Dl645FunctionCode::ClearMaxDemand => {
                    self.handle_clear_max_demand(security_params, address, version)
                        .await?;
                }
                Dl645FunctionCode::ClearMeter => {
                    self.handle_clear_meter(security_params, address, version)
                        .await?;
                }
                Dl645FunctionCode::ClearEvents => {
                    self.handle_clear_events(value, security_params, address, version)
                        .await?;
                }
                other => {
                    return Err(DriverError::ConfigurationError(format!(
                        "DL/T 645 execute_action unsupported function code: {:?}",
                        other
                    )));
                }
            }
        }

        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(json!({
                "status": "ok",
                "action": action.name,
            })),
        })
    }

    async fn write_point(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let d = device
            .downcast_ref::<Dl645Device>()
            .ok_or(DriverError::InvalidEntity(
                "Device is not a DL/T 645 device in this driver".to_string(),
            ))?;
        let point = point
            .downcast_ref::<Dl645Point>()
            .ok_or(DriverError::InvalidEntity(
                "Point is not a DL/T 645 point in this driver".to_string(),
            ))?;

        if !matches!(point.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            return Err(DriverError::ValidationError(
                "point is not writeable".to_string(),
            ));
        }

        let timeout_ms = timeout_ms
            .unwrap_or(self.inner.connection_policy.write_timeout_ms)
            .max(1);
        let timeout = Duration::from_millis(timeout_ms);
        let version = self.inner.config.version;
        let address = d.address;
        let security_params = d.security_params()?;

        // Per design: write_point is fixed to WriteData(DI).
        let param = Dl645Parameter {
            name: point.name.clone(),
            key: point.key.clone(),
            data_type: point.wire_data_type(),
            required: true,
            default_value: None,
            max_value: point.max_value,
            min_value: point.min_value,
            decimals: point.decimals,
            function_code: Dl645FunctionCode::WriteData,
            di: Some(point.di),
            transform: point.transform,
        };

        self.handle_write_data(security_params, &param, value, address, version, timeout)
            .await?;

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value.clone()),
        })
    }

    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        Ok(())
    }
}
