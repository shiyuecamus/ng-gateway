//! S7 southward data-plane handle.
//!
//! This is the **only** hot-path object published by the SDK supervision loop.
//! It MUST be cheap to clone (`Arc`), safe to use concurrently, and avoid extra allocations.
//!
//! Design:
//! - The protocol session (`protocol::session::Session`) is attached/detached by `S7Session`.
//! - Data-plane methods fail fast with `ServiceUnavailable` when no active session exists.

use super::{
    codec::S7Codec,
    protocol::{frame::S7TransportSize, session::Session as ProtoSession},
    types::{S7Action, S7Channel, S7Device, S7Parameter, S7Point},
};
use arc_swap::ArcSwapOption;
use chrono::Utc;
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, CollectItem, CollectionGroupKey, DeviceBuffers, DriverError,
    DriverResult, ExecuteOutcome, ExecuteResult, NGValue, NorthwardData, PointValue, RuntimeAction,
    RuntimeDelta, RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardHandle, WriteOutcome,
    WriteResult,
};
use std::{collections::HashMap, sync::Arc};
use tokio::time::timeout;
use tracing::instrument;

/// S7 data-plane handle published when the protocol session is Ready.
pub struct S7Handle {
    /// Typed runtime channel configuration.
    inner: Arc<S7Channel>,
    /// Current active protocol session (lock-free hot reads).
    session: ArcSwapOption<ProtoSession>,
}

impl S7Handle {
    /// Collection group key namespace for grouping by channel.
    ///
    /// ASCII: "S7CH"
    const KIND_S7_CHANNEL: u32 = 0x5337_4348;

    /// Create a new handle for the given channel (no I/O).
    #[inline]
    pub fn new(inner: Arc<S7Channel>) -> Self {
        Self {
            inner,
            session: ArcSwapOption::from(None),
        }
    }

    /// Attach a protocol session for this attempt.
    ///
    /// This is called exactly once per attempt, after the session becomes Active.
    #[inline]
    pub(crate) fn attach_session(&self, session: Arc<ProtoSession>) {
        self.session.store(Some(session));
    }

    /// Detach protocol session (best-effort).
    #[inline]
    pub(crate) fn detach_session(&self) {
        self.session.store(None);
    }

    #[inline]
    fn load_session(&self) -> DriverResult<Arc<ProtoSession>> {
        self.session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)
    }
}

#[async_trait::async_trait]
impl SouthwardHandle for S7Handle {
    fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        device
            .downcast_ref::<S7Device>()
            .map(|d| CollectionGroupKey::from_u64(Self::KIND_S7_CHANNEL, d.channel_id as u64))
    }

    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        if items.is_empty() {
            return Err(DriverError::ValidationError(
                "collect_data called with empty items".to_string(),
            ));
        }

        // Prepare per-device output buffers and build a merged point list.
        let mut buffers = HashMap::with_capacity(items.len());
        let mut s7_points = Vec::new();

        for (dev_any, points_any) in items.iter() {
            let dev = dev_any
                .downcast_ref::<S7Device>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not S7Device for S7Handle".into(),
                ))?;

            buffers
                .entry(dev.id)
                .or_insert(DeviceBuffers::new(dev.device_name.clone()));

            for p_any in points_any.iter() {
                let Ok(p) = Arc::clone(p_any).downcast_arc::<S7Point>() else {
                    continue;
                };
                if !p.readable() {
                    continue;
                }
                s7_points.push(p);
            }
        }

        if s7_points.is_empty() {
            return Ok(Vec::new());
        }

        let session = self.load_session()?;

        // Batch read across all points with an outer deadline that caps total
        // collect_data duration. Without this, N batches each consuming nearly
        // `read_timeout` could exceed the collection interval by N×.
        // Outer deadline = 3× read_timeout (min 5 s) to cap the total duration when
        // AMQ-bounded waves cause `ceil(N / AMQ) × read_timeout` latency.
        let collect_timeout = tokio::time::Duration::from_millis(
            self.inner
                .connection_policy
                .read_timeout_ms
                .saturating_mul(3)
                .max(5000),
        );
        let addresses = s7_points.iter().map(|p| &p.address).collect::<Vec<_>>();
        let results = match timeout(collect_timeout, session.read_addresses_typed(&addresses)).await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(DriverError::ExecutionError(e.to_string())),
            Err(_elapsed) => return Err(DriverError::Timeout(collect_timeout)),
        };

        for (p, it) in s7_points.iter().zip(results.into_iter()) {
            let Some(v) = it.value else {
                continue;
            };
            let Some(value) = S7Codec::to_value(&v, p.logical_data_type(), &p.transform) else {
                continue;
            };
            let Some(buf) = buffers.get_mut(&p.device_id) else {
                continue;
            };
            buf.push(
                p.r#type(),
                PointValue {
                    point_id: p.id,
                    point_key: Arc::<str>::from(p.key.as_str()),
                    value,
                    ts: None,
                },
            );
        }

        // Build stable, per-business-device outputs with a single group timestamp.
        let ts = Utc::now();
        let mut device_ids: Vec<i32> = buffers.keys().copied().collect();
        device_ids.sort_unstable();

        let mut out = Vec::with_capacity(device_ids.len() * 2);
        for device_id in device_ids {
            let Some(buf) = buffers.remove(&device_id) else {
                continue;
            };
            out.extend(buf.into_northward(device_id, ts));
        }

        Ok(out)
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
        let session = self.load_session()?;

        // Build write items without applying scale (per spec).
        let mut items = Vec::with_capacity(resolved.len());
        for (param, value) in resolved.iter() {
            let ts = S7TransportSize::try_from(param.address.transport_size).map_err(|_| {
                DriverError::ConfigurationError("Invalid transport size".to_string())
            })?;
            let val = S7Codec::from_value(value, ts)?;
            items.push((&param.address, val));
        }

        if let Err(e) = session.write_addresses_typed(&items).await {
            return Err(DriverError::ExecutionError(e.to_string()));
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
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let point = point
            .downcast_ref::<S7Point>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not S7Point for S7Handle".to_string(),
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

        let session = self.load_session()?;

        let ts = S7TransportSize::try_from(point.address.transport_size).map_err(|_| {
            DriverError::ConfigurationError("Invalid transport size in S7 address".to_string())
        })?;
        let s7_value = S7Codec::from_value(value, ts)?;
        let address = point.address.clone();

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

        write_res?;

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value.clone()),
        })
    }

    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        // TODO(delta): Implement when S7 needs dynamic runtime model updates.
        Ok(())
    }
}
