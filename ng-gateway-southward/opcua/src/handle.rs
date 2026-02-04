//! OPC UA southward data-plane handle.
//!
//! This is the **only** hot-path object published by the SDK supervision loop.
//! It provides:
//! - polling reads (`collect_data`)
//! - writes (`write_point`, `execute_action`)
//! - best-effort runtime delta application for subscription mode
//!
//! Connection lifecycle is governed by the SDK supervisor; the current `opcua::client::Session`
//! is attached/detached by `OpcUaSession`.

use super::{
    codec::OpcUaCodec,
    subscribe::{SubscriptionCommand, SubscriptionManager},
    types::{
        DeviceMeta, OpcUaAction, OpcUaChannel, OpcUaDevice, OpcUaParameter, OpcUaPoint,
        OpcUaReadMode, PointMeta, PointSnapshot,
    },
};
use arc_swap::{ArcSwap, ArcSwapOption};
use async_trait::async_trait;
use chrono::Utc;
use ng_gateway_sdk::{
    downcast_parameters, supervision::ReconnectHandle, AccessMode, CollectItem, CollectionGroupKey,
    CollectionType, DeviceBuffers, DriverError, DriverResult, ExecuteOutcome, ExecuteResult,
    NGValue, NorthwardData, NorthwardPublisher, PointValue, RuntimeAction, RuntimeDelta,
    RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardHandle, Status, WriteOutcome,
    WriteResult,
};
use opcua::{
    client::Session,
    types::{
        constants::MAX_ARRAY_LENGTH, DataValue, NodeId, ReadValueId, StatusCode,
        TimestampsToReturn, WriteValue,
    },
};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock, RwLock,
    },
};
use tokio::{sync::Mutex, task::JoinHandle, time::Duration as TokioDuration};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// OPC UA data-plane handle.
pub struct OpcUaHandle {
    /// Typed runtime channel configuration.
    inner: Arc<OpcUaChannel>,
    /// Current active OPC UA session (lock-free hot reads).
    session: ArcSwapOption<Session>,
    /// Supervisor reconnect handle (best-effort).
    reconnect: OnceLock<ReconnectHandle>,

    /// Effective max nodes per `Session::read()` call (0 means unknown).
    read_chunk_size: AtomicU64,
    /// Cached NodeIds parsed from string to avoid repeated parsing costs on hot path.
    node_id_cache: RwLock<HashMap<String, NodeId>>,

    /// Subscription mode config.
    subscribe_enabled: bool,
    subscribe_batch_size: usize,
    publisher: Arc<dyn NorthwardPublisher>,

    /// Snapshot used by subscription callbacks and runtime delta.
    snapshot: Option<Arc<ArcSwap<PointSnapshot>>>,
    /// Current subscription manager (attempt-scoped; replaced on reconnect).
    subs_mgr: Mutex<Option<Arc<SubscriptionManager>>>,
    /// Current subscription actor task handle (attempt-scoped).
    subs_task: Mutex<Option<JoinHandle<()>>>,
}

impl OpcUaHandle {
    /// Collection group key namespace for grouping by channel.
    ///
    /// ASCII: "OPCH"
    const KIND_OPCUA_CHANNEL: u32 = 0x4F50_4348;

    /// Create a new handle from init context (no I/O).
    pub fn new(
        inner: Arc<OpcUaChannel>,
        publisher: Arc<dyn NorthwardPublisher>,
        ctx: &ng_gateway_sdk::SouthwardInitContext,
    ) -> Self {
        let subscribe_enabled = inner.collection_type == CollectionType::Report
            && inner.config.read_mode == OpcUaReadMode::Subscribe;
        let snapshot = if subscribe_enabled {
            Some(Self::build_initial_snapshot_static(&inner, ctx))
        } else {
            None
        };

        Self {
            inner,
            session: ArcSwapOption::from(None),
            reconnect: std::sync::OnceLock::new(),
            read_chunk_size: AtomicU64::new(0),
            node_id_cache: RwLock::new(HashMap::new()),
            subscribe_enabled,
            subscribe_batch_size: ctx
                .runtime_channel
                .downcast_ref::<OpcUaChannel>()
                .map(|c| c.config.subscribe_batch_size.max(1))
                .unwrap_or(256),
            publisher,
            snapshot,
            subs_mgr: Mutex::new(None),
            subs_task: Mutex::new(None),
        }
    }

    /// Attach a connected session for this attempt.
    #[inline]
    pub(crate) fn attach_session(&self, session: Arc<Session>) {
        self.session.store(Some(session));
    }

    /// Detach session (best-effort).
    #[inline]
    pub(crate) fn detach_session(&self) {
        self.session.store(None);
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

    /// Best-effort update read chunk size from server capacity probe.
    pub(crate) fn set_read_chunk_size(&self, n: usize) {
        self.read_chunk_size
            .store(n.max(1) as u64, Ordering::Release);
    }

    /// (Re)create subscription manager for this attempt and spawn its actor task.
    pub(crate) async fn replace_subscription_manager(
        &self,
        cancel: CancellationToken,
    ) -> Option<Arc<SubscriptionManager>> {
        if !self.subscribe_enabled {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?.clone();
        let (mgr, actor) = SubscriptionManager::new_with_actor(
            cancel,
            Arc::clone(&self.publisher),
            snapshot,
            self.subscribe_batch_size.max(1),
        );
        let mgr = Arc::new(mgr);

        // Replace manager and task handle.
        *self.subs_mgr.lock().await = Some(Arc::clone(&mgr));
        // Preserve the current tracing span (contains `channel_id`) for the spawned task,
        // so per-channel dynamic log level overrides can work reliably.
        let task = tokio::spawn(async move { actor.run().await }.in_current_span());
        *self.subs_task.lock().await = Some(task);
        Some(mgr)
    }

    /// Build initial snapshot for subscription mode.
    fn build_initial_snapshot_static(
        _inner: &Arc<OpcUaChannel>,
        ctx: &ng_gateway_sdk::SouthwardInitContext,
    ) -> Arc<ArcSwap<PointSnapshot>> {
        // Cold-path init: pre-allocate to reduce HashMap rehashing and Vec growth.
        let approx_total_points = ctx
            .points_by_device
            .values()
            .map(|ps| ps.len())
            .sum::<usize>();

        let mut node_to_meta = HashMap::with_capacity(approx_total_points);
        let mut point_id_to_node = HashMap::with_capacity(approx_total_points);
        let mut device_to_nodes: HashMap<i32, Vec<NodeId>> =
            HashMap::with_capacity(ctx.points_by_device.len());

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

        // Parse NodeId without cache at init time (cold path).
        // Keep `device_to_nodes` grouped and pre-sized by device to reduce reallocations.
        for (dev_id, ps) in ctx.points_by_device.iter() {
            let mut nodes = Vec::with_capacity(ps.len());
            for p_any in ps.iter() {
                let Ok(p) = Arc::clone(p_any).downcast_arc::<OpcUaPoint>() else {
                    continue;
                };
                if !p.readable() {
                    continue;
                }
                let Ok(id) = NodeId::from_str(&p.node_id) else {
                    continue;
                };

                let meta = PointMeta {
                    device_id: p.device_id,
                    point: Arc::clone(&p),
                };
                node_to_meta.insert(id.clone(), meta);
                point_id_to_node.insert(p.id, id.clone());
                nodes.push(id);
            }
            if !nodes.is_empty() {
                device_to_nodes.insert(*dev_id, nodes);
            }
        }

        Arc::new(ArcSwap::from_pointee(PointSnapshot {
            node_to_meta,
            point_id_to_node,
            device_to_nodes,
            devices,
        }))
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
                w.entry(node_id_str.to_string()).or_insert(parsed.clone());
            }
            return Some(parsed);
        }
        None
    }

    #[inline]
    fn load_session(&self) -> DriverResult<Arc<Session>> {
        self.session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)
    }

    /// Apply point deltas to snapshot and return (creates, deletes) nodes for subscriptions.
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
            if !p.readable() {
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
                                list.swap_remove(pos);
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
                        list.swap_remove(pos);
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
}

#[async_trait]
impl SouthwardHandle for OpcUaHandle {
    fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        device
            .downcast_ref::<OpcUaDevice>()
            .map(|d| CollectionGroupKey::from_u64(Self::KIND_OPCUA_CHANNEL, d.channel_id as u64))
    }

    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        if items.is_empty() {
            return Err(DriverError::ValidationError(
                "collect_data called with empty items".to_string(),
            ));
        }

        let mut buffers = HashMap::with_capacity(items.len());
        let mut points = Vec::new();
        let mut nodes_to_read = Vec::new();

        for (dev_any, points_any) in items.iter() {
            let dev =
                dev_any
                    .downcast_ref::<OpcUaDevice>()
                    .ok_or(DriverError::ConfigurationError(
                        "RuntimeDevice is not OpcUaDevice for OpcUaHandle".to_string(),
                    ))?;

            buffers
                .entry(dev.id)
                .or_insert_with(|| DeviceBuffers::new(dev.device_name.clone()));

            for p_any in points_any.iter() {
                let Ok(p) = Arc::clone(p_any).downcast_arc::<OpcUaPoint>() else {
                    continue;
                };
                if !p.readable() {
                    continue;
                }
                let Some(id) = self.parse_node_id_cached(&p.node_id) else {
                    continue;
                };
                points.push(p);
                nodes_to_read.push(ReadValueId::new_value(id));
            }
        }

        if nodes_to_read.is_empty() {
            return Ok(Vec::new());
        }

        let max_read_nodes_per_call = match self.read_chunk_size.load(Ordering::Acquire) {
            0 => MAX_ARRAY_LENGTH.max(1),
            n => (n as usize).max(1),
        };

        let timeout_ms = self.inner.connection_policy.read_timeout_ms.max(1);
        let timeout_duration = TokioDuration::from_millis(timeout_ms);

        let session = self.load_session()?;

        let mut i = 0usize;
        while i < nodes_to_read.len() {
            let end = (i + max_read_nodes_per_call).min(nodes_to_read.len());
            let nodes_chunk = &nodes_to_read[i..end];
            let points_chunk = &points[i..end];

            let read_res: DriverResult<Vec<DataValue>> = match tokio::time::timeout(
                timeout_duration,
                session.read(nodes_chunk, TimestampsToReturn::Both, 0.0),
            )
            .await
            {
                Ok(Ok(values)) => Ok(values),
                Ok(Err(sc)) => Err(DriverError::ExecutionError(format!(
                    "OPC UA read status: {sc}"
                ))),
                Err(_) => {
                    self.try_request_reconnect("opcua read timeout");
                    Err(DriverError::Timeout(timeout_duration))
                }
            };

            let values = match read_res {
                Ok(v) => v,
                Err(e) => return Err(e),
            };

            for (p, dv) in points_chunk.iter().zip(values.iter()) {
                if dv.status.as_ref().map(|s| s.is_bad()).unwrap_or(false) {
                    continue;
                }
                let value_opt = dv.value.as_ref().and_then(|variant| {
                    OpcUaCodec::coerce_variant_value(variant, p.logical_data_type(), p.transform())
                });
                let Some(value) = value_opt else { continue };
                let Some(buf) = buffers.get_mut(&p.device_id) else {
                    continue;
                };
                buf.push(
                    p.r#type(),
                    PointValue {
                        point_id: p.id,
                        point_key: Arc::<str>::from(p.key.as_str()),
                        value,
                    },
                );
            }

            i = end;
        }

        let ts = Utc::now();
        let mut device_ids: Vec<i32> = buffers.keys().copied().collect();
        device_ids.sort_unstable();
        let mut out: Vec<NorthwardData> = Vec::with_capacity(device_ids.len() * 2);
        for device_id in device_ids {
            let Some(buf) = buffers.remove(&device_id) else {
                continue;
            };
            out.extend(buf.into_northward(device_id, ts));
        }
        Ok(out)
    }

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
        let timeout_duration = TokioDuration::from_millis(timeout_ms);

        let mut writes: Vec<WriteValue> = Vec::with_capacity(resolved.len());
        for (p, value) in resolved.into_iter() {
            let node_id = NodeId::from_str(&p.node_id).map_err(|_| {
                DriverError::ConfigurationError(format!(
                    "Invalid OPC UA node id for parameter '{}': {}",
                    p.key, p.node_id
                ))
            })?;
            let wire_dt = p.wire_data_type();
            let variant = OpcUaCodec::value_to_variant(&value, wire_dt).ok_or(
                DriverError::ValidationError(format!(
                    "OPC UA value conversion failed for parameter '{}': expected={:?}, actual={:?}, value={:?}",
                    p.key,
                    wire_dt,
                    value.data_type(),
                    value
                )),
            )?;
            writes.push(WriteValue::value_attr(node_id, variant));
        }

        let session = self.load_session()?;
        let res: DriverResult<Vec<StatusCode>> =
            match tokio::time::timeout(timeout_duration, session.write(&writes)).await {
                Ok(Ok(sc_list)) => Ok(sc_list),
                Ok(Err(sc)) => Err(DriverError::ExecutionError(format!(
                    "OPC UA write status: {sc}"
                ))),
                Err(_) => {
                    self.try_request_reconnect("opcua write timeout");
                    Err(DriverError::Timeout(timeout_duration))
                }
            };

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
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let point = point
            .downcast_ref::<OpcUaPoint>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not OpcUaPoint".to_string(),
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

        let node_id =
            self.parse_node_id_cached(&point.node_id)
                .ok_or(DriverError::ConfigurationError(format!(
                    "Invalid OPC UA node id for point '{}': {}",
                    point.key, point.node_id
                )))?;
        let wire_dt = point.wire_data_type();
        let variant = OpcUaCodec::value_to_variant(value, wire_dt).ok_or(
            DriverError::ValidationError(format!(
            "OPC UA value conversion failed for point '{}': expected={:?}, actual={:?}, value={:?}",
            point.key,
            wire_dt,
            value.data_type(),
            value
        )),
        )?;
        let write = WriteValue::value_attr(node_id, variant);

        let session = self.load_session()?;
        let res = if timeout_ms.is_some() {
            match tokio::time::timeout(timeout_duration, session.write(&[write])).await {
                Ok(inner) => inner.map_err(|e| DriverError::ExecutionError(e.to_string())),
                Err(_) => {
                    self.try_request_reconnect("opcua write_point timeout");
                    Err(DriverError::Timeout(timeout_duration))
                }
            }
        } else {
            session
                .write(&[write])
                .await
                .map_err(|e| DriverError::ExecutionError(e.to_string()))
        };

        let sc_list = res?;
        if sc_list.iter().any(|s| !s.is_good()) {
            return Err(DriverError::ExecutionError(format!(
                "OPC UA write failed: {:?}",
                sc_list
            )));
        }
        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value.clone()),
        })
    }

    async fn apply_runtime_delta(&self, delta: RuntimeDelta) -> DriverResult<()> {
        if !self.subscribe_enabled {
            return Ok(());
        }
        let snapshot_arc = match &self.snapshot {
            Some(s) => s,
            None => return Ok(()),
        };

        let mgr_opt = { self.subs_mgr.lock().await.clone() };
        let Some(mgr) = mgr_opt else { return Ok(()) };

        match delta {
            RuntimeDelta::DevicesChanged {
                added,
                updated,
                removed,
                status_changed,
            } => {
                // Update device metadata in the snapshot and map enable/disable to subscription membership.
                let old = snapshot_arc.load();
                let mut snap = (**old).clone();

                let mut creates: Vec<NodeId> = Vec::new();
                let mut deletes: Vec<NodeId> = Vec::new();

                let mut upsert_device = |d: &OpcUaDevice| {
                    snap.devices.insert(
                        d.id,
                        DeviceMeta {
                            name: d.device_name.clone(),
                            status: d.status(),
                        },
                    );
                };

                // Added/updated devices: keep metadata in sync (name/status).
                for d_any in added.iter().chain(updated.iter()) {
                    if let Some(d) = d_any.downcast_ref::<OpcUaDevice>() {
                        upsert_device(d);
                    }
                }

                // Removed devices: drop metadata and unsubscribe all its nodes.
                for d_any in removed.iter() {
                    if let Some(d) = d_any.downcast_ref::<OpcUaDevice>() {
                        snap.devices.remove(&d.id);
                        if let Some(nodes) = snap.device_to_nodes.remove(&d.id) {
                            deletes.extend(nodes.iter().cloned());
                            for n in nodes.iter() {
                                let _ = snap.node_to_meta.remove(n);
                            }
                            // Remove any point_id -> node mappings that refer to removed nodes.
                            let nodes_set: HashSet<NodeId> = nodes.into_iter().collect();
                            snap.point_id_to_node.retain(|_, v| !nodes_set.contains(v));
                        }
                    }
                }

                // Device status transitions: enable -> subscribe, disable -> unsubscribe.
                for (d_any, new_status) in status_changed.iter() {
                    if let Some(d) = d_any.downcast_ref::<OpcUaDevice>() {
                        let prev = snap.devices.get(&d.id).map(|m| m.status);
                        // Update snapshot status (and name, in case it drifted).
                        snap.devices.insert(
                            d.id,
                            DeviceMeta {
                                name: d.device_name.clone(),
                                status: *new_status,
                            },
                        );

                        if prev == Some(*new_status) {
                            continue;
                        }

                        if let Some(nodes) = snap.device_to_nodes.get(&d.id) {
                            match *new_status {
                                Status::Enabled => creates.extend(nodes.iter().cloned()),
                                _ => deletes.extend(nodes.iter().cloned()),
                            }
                        }
                    }
                }

                // Publish updated snapshot first so callbacks see fresh device metadata.
                snapshot_arc.store(Arc::new(snap));

                // Apply subscription changes.
                if !deletes.is_empty() {
                    mgr.send_command(SubscriptionCommand::DeleteNodes(deletes))
                        .await;
                }
                if !creates.is_empty() {
                    mgr.send_command(SubscriptionCommand::CreateNodes(creates))
                        .await;
                }
            }
            RuntimeDelta::PointsChanged {
                added,
                updated,
                removed,
                ..
            } => {
                let (creates, deletes) =
                    self.diff_points_delta(snapshot_arc, &added, &updated, &removed);
                if !deletes.is_empty() {
                    mgr.send_command(SubscriptionCommand::DeleteNodes(deletes))
                        .await;
                }
                if !creates.is_empty() {
                    mgr.send_command(SubscriptionCommand::CreateNodes(creates))
                        .await;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
