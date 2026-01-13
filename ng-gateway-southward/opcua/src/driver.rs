use super::{
    codec::OpcUaCodec,
    subscribe::{SubscriptionActor, SubscriptionCommand, SubscriptionManager},
    supervisor::{SessionEntry, SessionSupervisor, SharedSession},
    types::{
        DeviceMeta, OpcUaAction, OpcUaDevice, OpcUaParameter, OpcUaPoint, OpcUaReadMode, PointMeta,
        PointSnapshot,
    },
};
use crate::types::OpcUaChannel;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use chrono::Utc;
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, AttributeData, CollectionType, DataPointType, Driver,
    DriverError, DriverHealth, DriverResult, ExecuteOutcome, ExecuteResult, HealthStatus, NGValue,
    NorthwardData, PointValue, RuntimeAction, RuntimeDelta, RuntimeDevice, RuntimeParameter,
    RuntimePoint, SouthwardConnectionState, SouthwardInitContext, Status, TelemetryData,
    WriteOutcome, WriteResult,
};
use opcua::{
    client::Session,
    types::{DataValue, NodeId, ReadValueId, StatusCode, TimestampsToReturn, WriteValue},
};
use serde_json::json;
use std::{
    collections::HashMap,
    str::FromStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::{Duration as StdDuration, Instant},
};
use tokio::{sync::watch, task::JoinHandle, time::Duration as TokioDuration};
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

/// Production-grade OPC UA driver with session lifecycle, read/subscribe, and write
pub struct OpcUaDriver {
    /// Driver configuration
    inner: Arc<OpcUaChannel>,
    /// Session
    shared: SharedSession,
    /// Started flag to prevent duplicate start
    started: std::sync::atomic::AtomicBool,
    cancel_token: CancellationToken,

    /// Connection state channel for event-based state changes
    conn_tx: watch::Sender<SouthwardConnectionState>,
    conn_rx: watch::Receiver<SouthwardConnectionState>,

    /// Metrics
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    last_avg_response_time_ms: AtomicU64,

    /// Cached NodeIds parsed from string to avoid repeated parsing costs on hot path
    node_id_cache: RwLock<HashMap<String, NodeId>>,
    /// Subscription manager
    subs_mgr: Option<Arc<SubscriptionManager>>,
    /// Pending subscription actor to be spawned on start
    subs_actor: std::sync::Mutex<Option<SubscriptionActor>>,
    /// Snapshot used by subscribe callbacks and runtime delta
    snapshot: Option<Arc<ArcSwap<PointSnapshot>>>,
}

impl OpcUaDriver {
    #[inline]
    fn build_initial_snapshot(&self, ctx: &SouthwardInitContext) -> Arc<ArcSwap<PointSnapshot>> {
        let mut node_to_meta = HashMap::new();
        let mut point_id_to_node = HashMap::new();
        let mut device_to_nodes: HashMap<i32, Vec<NodeId>> = HashMap::new();
        let devices = ctx
            .devices
            .iter()
            .filter_map(|d| d.downcast_ref::<OpcUaDevice>())
            .map(|d| {
                (
                    d.id,
                    DeviceMeta {
                        name: d.device_name.clone(),
                        status: d.status(),
                    },
                )
            })
            .collect();

        ctx.points_by_device
            .values()
            .flat_map(|ps| ps.iter())
            .filter_map(|p_any| Arc::clone(p_any).downcast_arc::<OpcUaPoint>().ok())
            .filter(|p| matches!(p.access_mode(), AccessMode::Read | AccessMode::ReadWrite))
            .filter_map(|p| self.parse_node_id_cached(&p.node_id).map(|id| (p, id)))
            .for_each(|(p, id)| {
                let meta = PointMeta {
                    device_id: p.device_id,
                    point: Arc::clone(&p),
                };
                node_to_meta.insert(id.clone(), meta);
                point_id_to_node.insert(p.id, id.clone());
                device_to_nodes.entry(p.device_id).or_default().push(id);
            });

        Arc::new(ArcSwap::from_pointee(PointSnapshot {
            node_to_meta,
            point_id_to_node,
            device_to_nodes,
            devices,
        }))
    }

    #[inline]
    fn diff_points_delta(
        &self,
        snapshot: &ArcSwap<PointSnapshot>,
        added: &[Arc<dyn RuntimePoint>],
        updated: &[Arc<dyn RuntimePoint>],
        removed: &[Arc<dyn RuntimePoint>],
    ) -> (Vec<NodeId>, Vec<NodeId>) {
        let old = snapshot.load();
        let mut node_to_meta = old.node_to_meta.clone();
        let mut point_id_to_node = old.point_id_to_node.clone();
        let mut device_to_nodes = old.device_to_nodes.clone();
        let devices = old.devices.clone();

        let mut creates: Vec<NodeId> = Vec::new();
        let mut deletes: Vec<NodeId> = Vec::new();

        for p in added
            .iter()
            .filter_map(|p| Arc::clone(p).downcast_arc::<OpcUaPoint>().ok())
        {
            if !matches!(p.access_mode, AccessMode::Read | AccessMode::ReadWrite) {
                continue;
            }
            if let Some(dm) = devices.get(&p.device_id) {
                if dm.status != Status::Enabled {
                    continue;
                }
            }
            if let Some(id) = self.parse_node_id_cached(&p.node_id) {
                let meta = PointMeta {
                    device_id: p.device_id,
                    point: Arc::clone(&p),
                };
                node_to_meta.insert(id.clone(), meta);
                point_id_to_node.insert(p.id, id.clone());
                device_to_nodes
                    .entry(p.device_id)
                    .or_default()
                    .push(id.clone());
                creates.push(id);
            }
        }

        for p in updated
            .iter()
            .filter_map(|p| Arc::clone(p).downcast_arc::<OpcUaPoint>().ok())
        {
            if let Some(old_id) = point_id_to_node.get(&p.id).cloned() {
                if let Some(new_id) = self.parse_node_id_cached(&p.node_id) {
                    if new_id != old_id {
                        deletes.push(old_id.clone());
                        if let Some(dm) = devices.get(&p.device_id) {
                            if dm.status == Status::Enabled {
                                creates.push(new_id.clone());
                            }
                        } else {
                            creates.push(new_id.clone());
                        }
                        point_id_to_node.insert(p.id, new_id.clone());
                        if let Some(list) = device_to_nodes.get_mut(&p.device_id) {
                            if let Some(pos) = list.iter().position(|n| *n == old_id) {
                                let _ = list.remove(pos);
                            }
                            list.push(new_id.clone());
                        }
                        let _ = node_to_meta.remove(&old_id);
                        node_to_meta.insert(
                            new_id.clone(),
                            PointMeta {
                                device_id: p.device_id,
                                point: Arc::clone(&p),
                            },
                        );
                    } else {
                        node_to_meta.insert(
                            old_id,
                            PointMeta {
                                device_id: p.device_id,
                                point: Arc::clone(&p),
                            },
                        );
                    }
                }
            }
        }

        for p in removed
            .iter()
            .filter_map(|p| Arc::clone(p).downcast_arc::<OpcUaPoint>().ok())
        {
            if let Some(id) = point_id_to_node.remove(&p.id) {
                deletes.push(id.clone());
                let _ = node_to_meta.remove(&id);
                if let Some(list) = device_to_nodes.get_mut(&p.device_id) {
                    if let Some(pos) = list.iter().position(|n| *n == id) {
                        let _ = list.remove(pos);
                    }
                }
            }
        }

        snapshot.store(Arc::new(PointSnapshot {
            node_to_meta,
            point_id_to_node,
            device_to_nodes,
            devices,
        }));

        (creates, deletes)
    }

    /// Parse NodeId from string with an internal cache.
    /// Returns None if parsing fails.
    #[inline]
    fn parse_node_id_cached(&self, node_id_str: &str) -> Option<NodeId> {
        if let Ok(guard) = self.node_id_cache.read() {
            if let Some(id) = guard.get(node_id_str) {
                return Some(id.clone());
            }
        }
        if let Ok(parsed) = NodeId::from_str(node_id_str) {
            if let Ok(mut w) = self.node_id_cache.write() {
                w.entry(node_id_str.to_string())
                    .or_insert_with(|| parsed.clone());
            }
            return Some(parsed);
        }
        None
    }

    pub fn with_context(ctx: SouthwardInitContext) -> DriverResult<Self> {
        let (state_tx, state_rx) = watch::channel(SouthwardConnectionState::Disconnected);
        let inner = ctx
            .runtime_channel
            .clone()
            .downcast_arc::<OpcUaChannel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid OpcUaChannel".to_string()))?;

        let cancel_token = CancellationToken::new();
        let mut driver = Self {
            inner,
            shared: Arc::new(SessionEntry::new_empty()),
            started: std::sync::atomic::AtomicBool::new(false),
            cancel_token,
            conn_tx: state_tx,
            conn_rx: state_rx,
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            last_avg_response_time_ms: AtomicU64::new(0),
            node_id_cache: RwLock::new(HashMap::new()),
            subs_mgr: None,
            subs_actor: std::sync::Mutex::new(None),
            snapshot: None,
        };

        // Initialize SubscriptionManager for subscribe mode
        if driver.inner.collection_type == CollectionType::Report
            && driver.inner.config.read_mode == OpcUaReadMode::Subscribe
        {
            let snapshot = driver.build_initial_snapshot(&ctx);
            let (mgr, actor) = SubscriptionManager::new_with_actor(
                driver.cancel_token.child_token(),
                Arc::clone(&ctx.publisher),
                Arc::clone(&snapshot),
                driver.inner.config.subscribe_batch_size.max(1),
            );
            driver.subs_mgr = Some(Arc::new(mgr));
            driver.subs_actor = std::sync::Mutex::new(Some(actor));
            driver.snapshot = Some(snapshot);
        }

        Ok(driver)
    }

    #[inline]
    fn mark_unhealthy(&self, err_msg: Option<String>) {
        if let Some(msg) = err_msg {
            if let Ok(mut guard) = self.shared.last_error.lock() {
                *guard = Some(msg);
            }
        }
        self.shared.healthy.store(false, Ordering::Release);
    }

    async fn read_points(
        &self,
        device: &OpcUaDevice,
        points_any: &[Arc<dyn RuntimePoint>],
    ) -> DriverResult<Vec<NorthwardData>> {
        if points_any.is_empty() {
            return Ok(Vec::new());
        }
        // Single pass: downcast + access_mode filter + NodeId parse.
        // This keeps `(points[i], nodes_to_read[i])` aligned without allocating an intermediate Vec<(..,..)>.
        let mut points: Vec<Arc<OpcUaPoint>> = Vec::new();
        let mut nodes_to_read: Vec<ReadValueId> = Vec::new();
        for p_any in points_any.iter() {
            let Ok(p) = Arc::clone(p_any).downcast_arc::<OpcUaPoint>() else {
                continue;
            };
            if !matches!(p.access_mode(), AccessMode::Read | AccessMode::ReadWrite) {
                continue;
            }
            let Some(id) = self.parse_node_id_cached(&p.node_id) else {
                continue;
            };
            points.push(p);
            nodes_to_read.push(ReadValueId::new_value(id));
        }
        if nodes_to_read.is_empty() {
            return Ok(Vec::new());
        }

        let timeout_ms = self.inner.connection_policy.read_timeout_ms.max(1);

        let start_ts = Instant::now();
        let session_opt = self.shared.session.load_full();
        let timeout_duration = TokioDuration::from_millis(timeout_ms);
        let results: DriverResult<Vec<DataValue>> = match session_opt {
            None => Err(DriverError::ServiceUnavailable),
            Some(session) => {
                let session = Arc::clone(&session);
                match tokio::time::timeout(
                    timeout_duration,
                    session.read(&nodes_to_read, TimestampsToReturn::Both, 0.0),
                )
                .await
                {
                    Ok(Ok(values)) => Ok(values),
                    Ok(Err(sc)) => {
                        let msg = format!("OPC UA read status: {sc}");
                        Err(DriverError::ExecutionError(msg))
                    }
                    Err(_elapsed) => Err(DriverError::Timeout(timeout_duration)),
                }
            }
        };

        // Update unified metrics for read path
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        let elapsed_ms = start_ts.elapsed().as_millis() as u64;
        match &results {
            Ok(_) => {
                self.successful_requests.fetch_add(1, Ordering::Relaxed);
                let prev = self.last_avg_response_time_ms.load(Ordering::Acquire);
                let new_avg = if prev == 0 {
                    elapsed_ms
                } else {
                    (prev.saturating_mul(9) + elapsed_ms) / 10
                };
                self.last_avg_response_time_ms
                    .store(new_avg, Ordering::Release);
            }
            Err(_) => {
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
            }
        }

        let values = results?;
        if values.len() != points.len() {
            // Defensive: OPC UA servers *should* return one DataValue per requested node, in order.
            // If not, avoid out-of-bounds and surface a signal for troubleshooting.
            warn!(
                device_id = device.id,
                requested = points.len(),
                returned = values.len(),
                "OPC UA read returned unexpected value count"
            );
        }
        let mut telemetry_values: Vec<PointValue> = Vec::with_capacity(points.len());
        let mut attribute_values: Vec<PointValue> = Vec::with_capacity(points.len());

        for (p, dv) in points.iter().zip(values.iter()) {
            if dv.status.as_ref().map(|s| s.is_bad()).unwrap_or(false) {
                warn!(key = %p.key, status = ?dv.status, "OPC UA value status is bad");
                continue;
            }
            let value_opt = dv.value.as_ref().and_then(|variant| {
                OpcUaCodec::coerce_variant_value(variant, p.data_type(), p.scale())
            });
            let value = match value_opt {
                Some(v) => v,
                None => {
                    warn!(key = %p.key, expected = ?p.data_type, "OPC UA value type mismatch - dropped");
                    continue;
                }
            };
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

        let mut out = Vec::with_capacity(2);
        if !telemetry_values.is_empty() {
            out.push(NorthwardData::Telemetry(TelemetryData::new(
                device.id,
                device.device_name.clone(),
                telemetry_values,
            )));
        }
        if !attribute_values.is_empty() {
            out.push(NorthwardData::Attributes(
                AttributeData::new_client_attributes(
                    device.id,
                    device.device_name.clone(),
                    attribute_values,
                ),
            ));
        }
        Ok(out)
    }
}

#[async_trait]
impl Driver for OpcUaDriver {
    #[instrument(level = "info", skip_all)]
    async fn start(&self) -> DriverResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // already started; make idempotent
            return Ok(());
        }

        // Launch supervisor task that owns event loop and handles reconnects.
        let cancel = self.cancel_token.child_token();
        let supervisor =
            SessionSupervisor::new(Arc::clone(&self.shared), cancel, self.conn_tx.clone());
        let subs_mgr = self.subs_mgr.as_ref().map(Arc::clone);
        let inner = Arc::clone(&self.inner);

        // Spawn subscription actor if present
        if let Some(actor) = self.subs_actor.lock().unwrap().take() {
            tokio::spawn(async move {
                actor.run().await;
            });
        }

        let on_connected = subs_mgr.map(|mgr| {
            Box::new(move |session: Arc<Session>| {
                let mgr = Arc::clone(&mgr);
                tokio::spawn(async move {
                    mgr.send_command(SubscriptionCommand::NewSession(session))
                        .await;
                })
            }) as Box<dyn Fn(Arc<Session>) -> JoinHandle<()> + Send + Sync + 'static>
        });
        supervisor.run(inner, on_connected).await;
        Ok(())
    }

    #[instrument(level = "info", skip_all)]
    /// Stop driver and cleanup
    async fn stop(&self) -> DriverResult<()> {
        self.cancel_token.cancel();
        self.shared.shutdown.store(true, Ordering::Release);
        if let Some(session) = self.shared.session.load_full() {
            // Stop automatic reconnect and close the session
            let session = Arc::clone(&session);
            let _ = tokio::time::timeout(
                TokioDuration::from_secs(2),
                session.disconnect_without_delete_subscriptions(),
            )
            .await;
        }
        Ok(())
    }

    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn collect_data(
        &self,
        device: Arc<dyn RuntimeDevice>,
        data_points: Arc<[Arc<dyn RuntimePoint>]>,
    ) -> DriverResult<Vec<NorthwardData>> {
        if let Some(d) = device.downcast_ref::<OpcUaDevice>() {
            return self.read_points(d, data_points.as_ref()).await;
        }
        Err(DriverError::ConfigurationError(
            "RuntimeDevice is not OpcUaDevice for OpcUaDriver".to_string(),
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
        let _device =
            device
                .downcast_ref::<OpcUaDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not OpcUaDevice".to_string(),
                ))?;
        let action =
            action
                .downcast_ref::<OpcUaAction>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeAction is not OpcUaAction".to_string(),
                ))?;

        let resolved = downcast_parameters::<OpcUaParameter>(parameters)?;
        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);

        let mut writes: Vec<WriteValue> = Vec::with_capacity(resolved.len());
        for (p, value) in resolved.into_iter() {
            let node_id = NodeId::from_str(&p.node_id).map_err(|_| {
                DriverError::ConfigurationError(format!(
                    "Invalid OPC UA node id for parameter '{}': {}",
                    p.key, p.node_id
                ))
            })?;
            let variant = OpcUaCodec::value_to_variant(&value, p.data_type).ok_or(
                DriverError::ValidationError(format!(
                    "OPC UA value conversion failed for parameter '{}': expected={:?}, actual={:?}, value={:?}",
                    p.key,
                    p.data_type,
                    value.data_type(),
                    value
                ))
            )?;
            writes.push(WriteValue::value_attr(node_id, variant));
        }

        let start_ts = Instant::now();
        let session_opt = self.shared.session.load_full();
        let timeout_duration = TokioDuration::from_millis(timeout_ms);
        let res: DriverResult<Vec<StatusCode>> = match session_opt {
            None => Err(DriverError::ServiceUnavailable),
            Some(session) => {
                let session = Arc::clone(&session);
                match tokio::time::timeout(timeout_duration, session.write(&writes)).await {
                    Ok(Ok(sc_list)) => Ok(sc_list),
                    Ok(Err(sc)) => {
                        let msg = format!("OPC UA write status: {sc}");
                        Err(DriverError::ExecutionError(msg))
                    }
                    Err(_elapsed) => Err(DriverError::Timeout(timeout_duration)),
                }
            }
        };

        self.total_requests.fetch_add(1, Ordering::Relaxed);
        let elapsed_ms = start_ts.elapsed().as_millis() as u64;
        match &res {
            Ok(_) => {
                self.successful_requests.fetch_add(1, Ordering::Relaxed);
                let prev = self.last_avg_response_time_ms.load(Ordering::Acquire);
                let new_avg = if prev == 0 {
                    elapsed_ms
                } else {
                    (prev.saturating_mul(9) + elapsed_ms) / 10
                };
                self.last_avg_response_time_ms
                    .store(new_avg, Ordering::Release);
            }
            Err(e) => {
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
                if matches!(e, DriverError::Timeout(_)) {
                    self.mark_unhealthy(Some("timeout".to_string()));
                }
            }
        }

        let sc_list = res?;
        if sc_list.iter().any(|s| !s.is_good()) {
            return Err(DriverError::ExecutionError(format!(
                "Some writes failed: {:?}",
                sc_list
            )));
        }
        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(json!(format!("Action '{}' executed", action.name()))),
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
            .downcast_ref::<OpcUaPoint>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not OpcUaPoint for OpcUaDriver".to_string(),
            ))?;

        if !matches!(point.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            return Err(DriverError::ValidationError(
                "point is not writeable".to_string(),
            ));
        }

        let effective_timeout_ms = timeout_ms
            .unwrap_or(self.inner.connection_policy.write_timeout_ms)
            .max(1);
        let timeout_duration = TokioDuration::from_millis(effective_timeout_ms);

        // Strict datatype guard (core should already validate, keep driver defensive).
        if !value.validate_datatype(point.data_type) {
            return Err(DriverError::ValidationError(format!(
                "type mismatch: expected {:?}, got {:?}",
                point.data_type,
                value.data_type()
            )));
        }

        let node_id =
            self.parse_node_id_cached(&point.node_id)
                .ok_or(DriverError::ConfigurationError(format!(
                    "Invalid OPC UA node id for point '{}': {}",
                    point.key, point.node_id
                )))?;
        let variant = OpcUaCodec::value_to_variant(&value, point.data_type).ok_or(
            DriverError::ValidationError(format!(
            "OPC UA value conversion failed for point '{}': expected={:?}, actual={:?}, value={:?}",
            point.key,
            point.data_type,
            value.data_type(),
            value
        )),
        )?;
        let write = WriteValue::value_attr(node_id, variant);

        let start_ts = Instant::now();
        let session_opt = self.shared.session.load_full();
        let res = match session_opt {
            None => Err(DriverError::ServiceUnavailable),
            Some(session) => {
                let session = Arc::clone(&session);
                if timeout_ms.is_some() {
                    match tokio::time::timeout(timeout_duration, session.write(&[write])).await {
                        Ok(inner) => inner.map_err(|e| DriverError::ExecutionError(e.to_string())),
                        Err(_elapsed) => Err(DriverError::Timeout(timeout_duration)),
                    }
                } else {
                    // Use session's configured write timeout.
                    session
                        .write(&[write])
                        .await
                        .map_err(|e| DriverError::ExecutionError(e.to_string()))
                }
            }
        };

        // Update metrics similarly to execute_action.
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        let elapsed_ms = start_ts.elapsed().as_millis() as u64;
        match &res {
            Ok(_) => {
                self.successful_requests.fetch_add(1, Ordering::Relaxed);
                let prev = self.last_avg_response_time_ms.load(Ordering::Acquire);
                let new_avg = if prev == 0 {
                    elapsed_ms
                } else {
                    (prev.saturating_mul(9) + elapsed_ms) / 10
                };
                self.last_avg_response_time_ms
                    .store(new_avg, Ordering::Release);
            }
            Err(e) => {
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
                if matches!(e, DriverError::Timeout(_)) {
                    self.mark_unhealthy(Some("timeout".to_string()));
                }
            }
        }

        let sc_list = res?;
        if sc_list.iter().any(|s| !s.is_good()) {
            return Err(DriverError::ExecutionError(format!(
                "OPC UA write failed: {:?}",
                sc_list
            )));
        }

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
    async fn apply_runtime_delta(&self, delta: RuntimeDelta) -> DriverResult<()> {
        let snapshot_arc = match &self.snapshot {
            Some(s) => s,
            None => return Ok(()),
        };
        let mgr = match &self.subs_mgr {
            Some(mgr) => mgr,
            None => return Ok(()),
        };

        match delta {
            RuntimeDelta::DevicesChanged {
                added,
                updated,
                removed,
                status_changed,
            } => {
                let old = snapshot_arc.load();
                let mut to_delete: Vec<NodeId> = removed
                    .iter()
                    .filter_map(|d| d.downcast_ref::<OpcUaDevice>())
                    .flat_map(|dev| {
                        old.device_to_nodes
                            .get(&dev.id)
                            .into_iter()
                            .flat_map(|v| v.iter().cloned())
                    })
                    .collect();
                let mut to_create: Vec<NodeId> = Vec::new();

                let node_to_meta = old.node_to_meta.clone();
                let point_id_to_node = old.point_id_to_node.clone();
                let mut device_to_nodes = old.device_to_nodes.clone();
                let mut devices = old.devices.clone();

                for d in added.iter().filter_map(|d| d.downcast_ref::<OpcUaDevice>()) {
                    devices.insert(
                        d.id,
                        DeviceMeta {
                            name: d.device_name.clone(),
                            status: d.status(),
                        },
                    );
                }
                for d in updated
                    .iter()
                    .filter_map(|d| d.downcast_ref::<OpcUaDevice>())
                {
                    // Preserve status if present; otherwise take current reported status
                    let status = devices
                        .get(&d.id)
                        .map(|m| m.status)
                        .unwrap_or_else(|| d.status());
                    devices.insert(
                        d.id,
                        DeviceMeta {
                            name: d.device_name.clone(),
                            status,
                        },
                    );
                }
                for d in removed
                    .iter()
                    .filter_map(|d| d.downcast_ref::<OpcUaDevice>())
                {
                    devices.remove(&d.id);
                    device_to_nodes.remove(&d.id);
                }

                // Handle explicit status changes
                for (dev_any, new_status) in status_changed.into_iter() {
                    if let Some(d) = dev_any.downcast_ref::<OpcUaDevice>() {
                        if let Some(dm) = devices.get_mut(&d.id) {
                            dm.name = d.device_name.clone();
                            dm.status = new_status;
                        } else {
                            devices.insert(
                                d.id,
                                DeviceMeta {
                                    name: d.device_name.clone(),
                                    status: new_status,
                                },
                            );
                        }
                        if let Some(nodes) = device_to_nodes.get(&d.id) {
                            match new_status {
                                Status::Disabled => to_delete.extend(nodes.iter().cloned()),
                                Status::Enabled => to_create.extend(nodes.iter().cloned()),
                            }
                        }
                    }
                }

                snapshot_arc.store(Arc::new(PointSnapshot {
                    node_to_meta,
                    point_id_to_node,
                    device_to_nodes,
                    devices,
                }));

                if !to_delete.is_empty() {
                    mgr.send_command(SubscriptionCommand::DeleteNodes(to_delete))
                        .await;
                }
                if !to_create.is_empty() {
                    mgr.send_command(SubscriptionCommand::CreateNodes(to_create))
                        .await;
                }
            }
            RuntimeDelta::PointsChanged {
                device: _,
                added,
                updated,
                removed,
            } => {
                let (creates, deletes) =
                    self.diff_points_delta(snapshot_arc.as_ref(), &added, &updated, &removed);
                if !deletes.is_empty() {
                    mgr.send_command(SubscriptionCommand::DeleteNodes(deletes))
                        .await;
                }
                if !creates.is_empty() {
                    mgr.send_command(SubscriptionCommand::CreateNodes(creates))
                        .await;
                }
            }
            RuntimeDelta::ActionsChanged { .. } => {}
        }
        Ok(())
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
            error_count: self.failed_requests.load(Ordering::Acquire),
            success_rate: {
                let total = self.total_requests.load(Ordering::Acquire) as f64;
                if total > 0.0 {
                    self.successful_requests.load(Ordering::Acquire) as f64 / total
                } else {
                    0.0
                }
            },
            average_response_time: StdDuration::from_millis(
                self.last_avg_response_time_ms.load(Ordering::Acquire),
            ),
            details: None,
        })
    }
}
