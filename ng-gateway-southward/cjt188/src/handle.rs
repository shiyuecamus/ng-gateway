//! CJ/T 188 southward data-plane handle.
//!
//! This is the **only** hot-path object published by the SDK supervision loop.
//! It MUST be cheap to clone (`Arc`), safe to use concurrently, and avoid extra allocations.
//!
//! Design:
//! - The transport is established by `Cjt188Connector`.
//! - A concrete protocol session (`dyn protocol::session::Cjt188Session`) is attached/detached by
//!   `Cjt188Session` (supervision layer).
//! - Data-plane methods fail fast with `ServiceUnavailable` when no active session exists.

use super::{
    codec::DIResponseParser,
    protocol::{
        error::ProtocolError,
        frame::defs::DataIdentifier,
        session::{Cjt188Session as ProtoSession, ReadDataParams},
    },
    types::{Cjt188Channel, Cjt188Device, Cjt188Point},
};
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use ng_gateway_sdk::{
    supervision::ReconnectHandle, AttributeData, CollectItem, DataPointType, DriverError,
    DriverResult, ExecuteResult, NGValue, NorthwardData, PointValue, RuntimeAction, RuntimeDelta,
    RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardHandle, TelemetryData, WriteResult,
};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

/// Concrete session wrapper needed for `ArcSwapOption` (trait objects are unsized).
pub struct SessionHandle(pub Arc<dyn ProtoSession>);

/// CJ/T 188 data-plane handle.
pub struct Cjt188Handle {
    inner: Arc<Cjt188Channel>,
    session: ArcSwapOption<SessionHandle>,
    timeout_reconnect_threshold: u32,
    consecutive_timeouts: AtomicU64,
    reconnect: OnceLock<ReconnectHandle>,
}

impl Cjt188Handle {
    /// Create a new handle for the given channel (no I/O).
    #[inline]
    pub fn new(inner: Arc<Cjt188Channel>) -> Self {
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
                let count = self
                    .consecutive_timeouts
                    .fetch_add(1, Ordering::Relaxed)
                    .saturating_add(1);
                let threshold = self.timeout_reconnect_threshold as u64;
                if threshold != 0 && count >= threshold {
                    self.try_request_reconnect("cjt188 timeout threshold reached");
                    self.consecutive_timeouts.store(0, Ordering::Release);
                }
            }
            ProtocolError::Transport(_) | ProtocolError::Io(_) => {
                self.try_request_reconnect("cjt188 transport/io error");
            }
            _ => {}
        }
    }

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
                self.try_request_reconnect("cjt188 no active session");
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
                Err(DriverError::ExecutionError(proto_err.to_string()))
            }
            Err(_) => {
                let proto_err =
                    ProtocolError::Timeout(format!("Operation timed out after {:?}", op_timeout));
                self.on_proto_error(&proto_err);
                Err(DriverError::ExecutionError(proto_err.to_string()))
            }
        }
    }
}

#[async_trait]
impl SouthwardHandle for Cjt188Handle {
    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        let (device, data_points) = items.first().ok_or(DriverError::ValidationError(
            "collect_data called with empty items".to_string(),
        ))?;
        if items.len() != 1 {
            return Err(DriverError::ConfigurationError(
                "CJ/T 188 driver does not support grouped collection".to_string(),
            ));
        }

        let d = device
            .downcast_ref::<Cjt188Device>()
            .ok_or(DriverError::InvalidEntity(
                "Device is not a CJ/T 188 device in this driver".to_string(),
            ))?;

        let address = d.address_struct()?;
        let meter_type = d.meter_type;

        let concrete_points = data_points
            .iter()
            .filter_map(|p| p.downcast_ref::<Cjt188Point>())
            .collect::<Vec<_>>();
        if concrete_points.is_empty() {
            return Ok(Vec::new());
        }

        // Group points by DI so we can perform a minimal set of reads and then parse fields.
        let mut points_by_di: HashMap<u16, Vec<&Cjt188Point>> = HashMap::new();
        for p in &concrete_points {
            points_by_di.entry(p.di).or_default().push(p);
        }

        let mut results: Vec<NorthwardData> = Vec::new();

        for (di, points_in_group) in points_by_di {
            let timeout_ms = self.inner.connection_policy.read_timeout_ms.max(1);
            let timeout = Duration::from_millis(timeout_ms);
            let di_enum = DataIdentifier::from(di);

            let resp = self
                .run_op(timeout, "read_data", move |session| async move {
                    session
                        .read_data(
                            ReadDataParams {
                                meter_type,
                                address,
                                di: di_enum,
                            },
                            timeout,
                        )
                        .await
                })
                .await?;

            let parsed =
                match DIResponseParser::parse(di, meter_type, &resp.payload, &points_in_group) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!(
                            device_id = d.id,
                            di = format!("0x{:04X}", di),
                            error = %e,
                            "Failed to parse CJ/T 188 DI response"
                        );
                        continue;
                    }
                };

            for point in points_in_group {
                let Some(v) = parsed.point_values.get(&point.id) else {
                    tracing::warn!(
                        device_id = d.id,
                        point_id = point.id,
                        point_key = %point.key,
                        field_key = %point.field_key,
                        di = format!("0x{:04X}", di),
                        "Point value not produced from DI response"
                    );
                    continue;
                };

                let pv = PointValue {
                    point_id: point.id,
                    point_key: Arc::<str>::from(point.key.as_str()),
                    value: v.clone(),
                    ts: None,
                };
                match point.r#type {
                    DataPointType::Telemetry => {
                        results.push(NorthwardData::Telemetry(TelemetryData::new(
                            d.id,
                            d.device_name.clone(),
                            vec![pv],
                        )));
                    }
                    DataPointType::Attribute => {
                        results.push(NorthwardData::Attributes(
                            AttributeData::new_client_attributes(
                                d.id,
                                d.device_name.clone(),
                                vec![pv],
                            ),
                        ));
                    }
                }
            }
        }

        Ok(results)
    }

    async fn execute_action(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        _action: Arc<dyn RuntimeAction>,
        _parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        Err(DriverError::ConfigurationError(
            "CJ/T 188 driver does not support execute_action (downlink is not implemented)"
                .to_string(),
        ))
    }

    async fn write_point(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        _point: Arc<dyn RuntimePoint>,
        _value: &NGValue,
        _timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        Err(DriverError::ConfigurationError(
            "CJ/T 188 driver does not support write_point (downlink is not implemented)"
                .to_string(),
        ))
    }

    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        Ok(())
    }
}
