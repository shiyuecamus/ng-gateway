use crate::types::{MonitoredItemFailureKind, PointSnapshot};
use arc_swap::ArcSwap;
use ng_gateway_sdk::{
    AttributeData, DataPointType, NorthwardData, NorthwardPublisher, PointValue, Status,
    TelemetryData,
};
use opcua::{
    client::{MonitoredItem, Session, SubscriptionCallbacks},
    types::{
        enums::MonitoringMode, DataValue, MonitoredItemCreateRequest, MonitoringParameters, NodeId,
        ReadValueId, TimestampsToReturn, Variant,
    },
};
use std::{collections::HashMap, sync::Arc, time::Duration as StdDuration};
use tokio::sync::mpsc::{self};
use tokio_util::sync::CancellationToken;

/// Subscription-related capacity limits reported by the OPC UA server.
///
/// This structure represents a subset of server capabilities that are
/// relevant for planning subscription layouts and putting soft guards
/// around the number of subscriptions / monitored items we try to register.
#[derive(Debug, Clone, Default)]
struct SubscriptionCapacity {
    /// Maximum number of sessions the server is willing to maintain.
    pub max_sessions: Option<u32>,
    /// Maximum total subscriptions for the server.
    pub max_subscriptions: Option<u32>,
    /// Maximum total monitored items for the whole server.
    pub max_monitored_items: Option<u32>,
    /// Maximum number of subscriptions per session.
    pub max_subscriptions_per_session: Option<u32>,
    /// Maximum monitored items per subscription. None means "unknown / unlimited".
    pub max_monitored_items_per_subscription: Option<u32>,
}

/// Server-side monitored item info
#[allow(unused)]
#[derive(Debug, Clone)]
struct MonitoredItemInfo {
    /// Identifier of the monitored item on the server side
    monitored_item_id: u32,
    /// Client handle used when creating the monitored item
    client_handle: u32,
    /// Revised sampling interval in milliseconds
    revised_sampling_ms: u64,
    /// Revised queue size negotiated with the server
    revised_queue_size: u32,
    /// Node id this monitored item is bound to
    node_id: NodeId,
}

/// Subscription manager commands
pub(super) enum SubscriptionCommand {
    /// A new connected session arrived
    NewSession(Arc<Session>),
    /// Create monitored items for nodes
    CreateNodes(Vec<NodeId>),
    /// Delete monitored items for nodes
    DeleteNodes(Vec<NodeId>),
}

/// State for a single OPC UA subscription on the server.
///
/// A session may host multiple subscriptions. Each subscription owns a
/// distinct set of monitored items identified by `node_id`.
#[derive(Debug, Clone, Default)]
struct SubscriptionState {
    /// Server-side subscription id
    id: u32,
    /// Mapping from node id to monitored item metadata for this subscription
    items: HashMap<NodeId, MonitoredItemInfo>,
}

/// Actor that owns all subscription-related state and reacts to `SubscriptionCommand`s.
///
/// A single actor instance is created per OPC UA session manager and lives until
/// the cancellation token is triggered or the command channel is closed.
pub(super) struct SubscriptionActor {
    /// Command receiver
    rx: mpsc::Receiver<SubscriptionCommand>,
    /// Cancellation token shared with the driver / supervisor
    cancel: CancellationToken,
    /// Northward publisher used by subscription callbacks
    publisher: Arc<dyn NorthwardPublisher>,
    /// Shared point snapshot used by callbacks and planning logic
    snapshot: Arc<ArcSwap<PointSnapshot>>,
    /// Batch size for monitored item creation
    batch_size: usize,

    /// Current active OPC UA session, if any
    session: Option<Arc<Session>>,
    /// Current subscriptions created on the server
    subscriptions: Vec<SubscriptionState>,
    /// Monotonically increasing client handle used for monitored items
    next_handle: u32,
    /// Cached server capacity limits
    capacity: SubscriptionCapacity,
}

impl SubscriptionActor {
    /// Main event loop that processes subscription commands until cancellation or channel close.
    pub(super) async fn run(mut self) {
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    return;
                }
                maybe_cmd = self.rx.recv() => {
                    let Some(cmd) = maybe_cmd else { return; };
                    match cmd {
                        SubscriptionCommand::NewSession(session) => {
                            self.handle_new_session(session).await;
                        }
                        SubscriptionCommand::CreateNodes(nodes) => {
                            self.handle_create_nodes(nodes).await;
                        }
                        SubscriptionCommand::DeleteNodes(nodes) => {
                            self.handle_delete_nodes(nodes).await;
                        }
                    }
                }
            }
        }
    }

    /// Handle a new connected session: read capacity and rebuild subscriptions.
    async fn handle_new_session(&mut self, session: Arc<Session>) {
        self.session = Some(Arc::clone(&session));
        // Read server-side capacity in a best-effort fashion.
        self.capacity = read_subscription_capacity(&session).await;

        if let Err(e) = self.rebuild_subscriptions_for_session(&session).await {
            tracing::error!(
                error = %e,
                "Failed to rebuild OPC UA subscriptions for session"
            );
        }
    }

    /// Handle runtime node creation requests from deltas.
    async fn handle_create_nodes(&mut self, nodes: Vec<NodeId>) {
        let Some(session_arc) = &self.session else {
            tracing::debug!(
                node_count = nodes.len(),
                "CreateNodes command deferred: session not available"
            );
            return;
        };
        // Clone the session Arc so we can use &mut self freely below without
        // holding an immutable borrow of `self.session`.
        let session = Arc::clone(session_arc);

        // If there are no active subscriptions yet but we already have a session,
        // rebuild subscriptions from the latest snapshot, which now includes the
        // newly added points. This ensures that newly imported points are picked
        // up immediately without waiting for a reconnect.
        if self.subscriptions.is_empty() {
            if let Err(e) = self.rebuild_subscriptions_for_session(&session).await {
                tracing::error!(
                    error = %e,
                    "Failed to rebuild OPC UA subscriptions for CreateNodes"
                );
            }
            return;
        }

        if let Err(e) = self.add_nodes_to_subscriptions(&session, nodes).await {
            tracing::error!(
                error = %e,
                "Failed to add nodes to OPC UA subscriptions"
            );
        }
    }

    /// Handle runtime node deletion requests from deltas.
    async fn handle_delete_nodes(&mut self, nodes: Vec<NodeId>) {
        let Some(session) = &self.session else {
            return;
        };
        if self.subscriptions.is_empty() {
            return;
        }

        // Group monitored item ids to delete by subscription id.
        let mut per_sub_ids: HashMap<u32, Vec<u32>> = HashMap::new();
        for node in nodes.into_iter() {
            for sub in self.subscriptions.iter_mut() {
                if let Some(mi) = sub.items.remove(&node) {
                    per_sub_ids
                        .entry(sub.id)
                        .or_default()
                        .push(mi.monitored_item_id);
                    break;
                }
            }
        }

        // Delete monitored items from server
        for (sid, ids) in per_sub_ids.iter() {
            if ids.is_empty() {
                continue;
            }
            if let Err(e) = session.delete_monitored_items(*sid, ids).await {
                tracing::warn!(
                    subscription_id = sid,
                    error = %e,
                    "Failed to delete OPC UA monitored items"
                );
            }
        }

        // Clean up empty subscriptions to avoid resource leaks and capacity waste
        let empty_sub_ids: Vec<u32> = self
            .subscriptions
            .iter()
            .filter(|sub| sub.items.is_empty())
            .map(|sub| sub.id)
            .collect();

        if !empty_sub_ids.is_empty() {
            for sid in empty_sub_ids.iter() {
                if let Err(e) = session.delete_subscription(*sid).await {
                    tracing::warn!(
                        subscription_id = sid,
                        error = %e,
                        "Failed to delete empty OPC UA subscription"
                    );
                } else {
                    tracing::info!(subscription_id = sid, "Empty OPC UA subscription deleted");
                }
            }

            // Remove deleted subscriptions from local state
            self.subscriptions
                .retain(|sub| !empty_sub_ids.contains(&sub.id));
        }
    }

    /// Rebuild all subscriptions for a session based on the current snapshot and capacity.
    ///
    /// This function:
    /// - Computes the full set of enabled nodes from the snapshot.
    /// - Applies total monitored items limit from capacity if available.
    /// - Shards nodes across multiple subscriptions based on
    ///   `max_monitored_items_per_subscription` and `max_subscriptions_per_session`.
    /// - For each subscription, creates it on the server and registers its nodes using
    ///   `register_nodes_chunked`.
    async fn rebuild_subscriptions_for_session(
        &mut self,
        session: &Arc<Session>,
    ) -> Result<(), String> {
        self.subscriptions.clear();

        let snap = self.snapshot.load();
        let mut all_nodes = Vec::new();
        for (dev_id, list) in snap.device_to_nodes.iter() {
            let enabled = snap
                .devices
                .get(dev_id)
                .map(|m| m.status == Status::Enabled)
                .unwrap_or(false);
            if enabled {
                all_nodes.extend(list.iter().cloned());
            }
        }

        if all_nodes.is_empty() {
            tracing::info!("No enabled OPC UA nodes found in snapshot; no subscriptions created");
            return Ok(());
        }

        // Apply total monitored items limit, if the server reports one.
        if let Some(total_limit) = self.capacity.max_monitored_items {
            if all_nodes.len() > total_limit as usize {
                tracing::warn!(
                    requested = all_nodes.len(),
                    max_monitored_items_total = total_limit,
                    "Total monitored items truncated to respect MaxMonitoredItems"
                );
                all_nodes.truncate(total_limit as usize);
            }
        }

        // Determine per-subscription capacity. If the server does not report a
        // per-subscription limit, we fall back to placing all nodes into a single
        // subscription (which matches the previous behavior).
        let per_sub_limit = match self.capacity.max_monitored_items_per_subscription {
            Some(v) if v > 0 => v as usize,
            _ => all_nodes.len().max(1),
        };

        let max_subs_per_session = self
            .capacity
            .max_subscriptions_per_session
            .unwrap_or(u32::MAX) as usize;

        let mut cursor = 0usize;
        let total_nodes = all_nodes.len();
        let mut created_sub_count = 0usize;

        while cursor < total_nodes && self.subscriptions.len() < max_subs_per_session {
            let end = (cursor + per_sub_limit).min(total_nodes);
            let nodes_slice = &all_nodes[cursor..end];
            if nodes_slice.is_empty() {
                break;
            }

            let callbacks = make_callbacks(Arc::clone(&self.publisher), Arc::clone(&self.snapshot));
            let res = session
                .create_subscription(StdDuration::from_millis(500), 60, 20, 0, 0, true, callbacks)
                .await;

            let sid = match res {
                Ok(sid) => sid,
                Err(e) => {
                    return Err(format!("Failed to create OPC UA subscription: {e}"));
                }
            };

            tracing::info!(
                subscription_id = sid,
                node_count = nodes_slice.len(),
                "OPC UA subscription created for sharded nodes"
            );

            let mut state = SubscriptionState {
                id: sid,
                items: HashMap::new(),
            };

            register_nodes_chunked(
                session,
                sid,
                nodes_slice,
                self.batch_size,
                &mut state.items,
                &mut self.next_handle,
            )
            .await;

            tracing::info!(
                subscription_id = sid,
                registered_count = state.items.len(),
                "OPC UA subscription nodes registered"
            );

            self.subscriptions.push(state);
            created_sub_count += 1;
            cursor = end;
        }

        if cursor < total_nodes {
            tracing::warn!(
                total_nodes,
                assigned = cursor,
                max_subscriptions_per_session = max_subs_per_session,
                per_subscription_limit = per_sub_limit,
                "Not all OPC UA nodes could be assigned to subscriptions due to server limits"
            );
        }

        tracing::info!(
            total_nodes,
            created_subscriptions = created_sub_count,
            "OPC UA subscriptions rebuilt for session"
        );

        Ok(())
    }

    /// Add nodes to existing subscriptions, creating additional subscriptions on demand
    /// as long as capacity limits allow.
    #[allow(clippy::too_many_arguments)]
    async fn add_nodes_to_subscriptions(
        &mut self,
        session: &Arc<Session>,
        nodes: Vec<NodeId>,
    ) -> Result<(), String> {
        if nodes.is_empty() {
            return Ok(());
        }

        // Filter out nodes that are already subscribed in any subscription.
        let mut pending = Vec::new();
        'outer: for n in nodes.into_iter() {
            for sub in self.subscriptions.iter() {
                if sub.items.contains_key(&n) {
                    continue 'outer;
                }
            }
            pending.push(n);
        }

        if pending.is_empty() {
            tracing::debug!("CreateNodes command ignored: all nodes already registered");
            return Ok(());
        }

        let per_sub_limit_opt = self.capacity.max_monitored_items_per_subscription;
        let max_subs_per_session = self
            .capacity
            .max_subscriptions_per_session
            .unwrap_or(u32::MAX) as usize;

        // Respect total monitored items limit if the server reports one.
        if let Some(total_limit) = self.capacity.max_monitored_items {
            let current_total: usize = self.subscriptions.iter().map(|s| s.items.len()).sum();
            if current_total >= total_limit as usize {
                tracing::warn!(
                  current_total,
                  max_monitored_items_total = total_limit,
                  "CreateNodes ignored: total monitored items already at or above MaxMonitoredItems"
              );
                return Ok(());
            }

            let allowed_new = (total_limit as usize).saturating_sub(current_total);
            if pending.len() > allowed_new {
                tracing::warn!(
                    requested = pending.len(),
                    allowed_new,
                    max_monitored_items_total = total_limit,
                    "CreateNodes truncated to respect MaxMonitoredItems"
                );
                pending.truncate(allowed_new);
            }
        }

        let mut cursor = 0usize;

        // First, try to fill existing subscriptions up to their per-subscription limit.
        if let Some(per_sub_limit) = per_sub_limit_opt {
            let per_sub_limit = per_sub_limit as usize;
            for sub in self.subscriptions.iter_mut() {
                if cursor >= pending.len() {
                    break;
                }
                let current = sub.items.len();
                if current >= per_sub_limit {
                    continue;
                }
                let remaining = per_sub_limit.saturating_sub(current);
                let end = (cursor + remaining).min(pending.len());
                let slice = &pending[cursor..end];
                if slice.is_empty() {
                    continue;
                }

                tracing::debug!(
                    subscription_id = sub.id,
                    node_count = slice.len(),
                    "Registering additional OPC UA monitored items in existing subscription"
                );
                register_nodes_chunked(
                    session,
                    sub.id,
                    slice,
                    self.batch_size,
                    &mut sub.items,
                    &mut self.next_handle,
                )
                .await;
                cursor = end;
            }
        } else if let Some(first) = self.subscriptions.first_mut() {
            // No per-subscription limit reported; append everything to the first
            // subscription, which matches the previous behavior.
            let slice = &pending[cursor..];
            tracing::debug!(
              subscription_id = first.id,
              node_count = slice.len(),
              "Registering OPC UA monitored items in single subscription (no per-sub limit reported)"
          );
            register_nodes_chunked(
                session,
                first.id,
                slice,
                self.batch_size,
                &mut first.items,
                &mut self.next_handle,
            )
            .await;
            cursor = pending.len();
        }

        // If there are still nodes left and the server reports a per-subscription limit,
        // create new subscriptions as long as the per-session subscription limit allows.
        if let Some(per_sub_limit) = per_sub_limit_opt {
            let per_sub_limit = per_sub_limit as usize;
            while cursor < pending.len() && self.subscriptions.len() < max_subs_per_session {
                let end = (cursor + per_sub_limit).min(pending.len());
                let slice = &pending[cursor..end];
                if slice.is_empty() {
                    break;
                }

                let callbacks =
                    make_callbacks(Arc::clone(&self.publisher), Arc::clone(&self.snapshot));
                let res = session
                    .create_subscription(
                        StdDuration::from_millis(500),
                        60,
                        20,
                        0,
                        0,
                        true,
                        callbacks,
                    )
                    .await;

                let sid = match res {
                    Ok(sid) => sid,
                    Err(e) => {
                        return Err(format!(
                            "Failed to create OPC UA subscription for CreateNodes: {e}"
                        ));
                    }
                };

                tracing::info!(
                    subscription_id = sid,
                    node_count = slice.len(),
                    "OPC UA subscription created for CreateNodes sharding"
                );

                let mut state = SubscriptionState {
                    id: sid,
                    items: HashMap::new(),
                };

                register_nodes_chunked(
                    session,
                    sid,
                    slice,
                    self.batch_size,
                    &mut state.items,
                    &mut self.next_handle,
                )
                .await;

                tracing::info!(
                    subscription_id = sid,
                    registered_count = state.items.len(),
                    "OPC UA subscription nodes registered for CreateNodes"
                );

                self.subscriptions.push(state);
                cursor = end;
            }
        }

        if cursor < pending.len() {
            tracing::warn!(
              remaining = pending.len() - cursor,
              "Some OPC UA nodes from CreateNodes could not be assigned to subscriptions due to capacity or implementation limits"
          );
        }

        Ok(())
    }
}

/// Best-effort helper to read subscription-related capacity limits from the server.
///
/// This function uses standard `Server_ServerCapabilities_*` nodes to obtain
/// maximum monitored item counts. Any read error or unexpected value type is
/// treated as "unknown" and logged, but never fails the caller.
#[inline]
async fn read_subscription_capacity(session: &Arc<Session>) -> SubscriptionCapacity {
    // Standard numeric node ids from async-opcua-types generated node_ids.rs:
    // - Server_ServerCapabilities_MaxSessions = 24095 (ns=0, i=24095)
    // - Server_ServerCapabilities_MaxSubscriptions = 24096 (ns=0, i=24096)
    // - Server_ServerCapabilities_MaxMonitoredItems = 24097 (ns=0, i=24097)
    // - Server_ServerCapabilities_MaxSubscriptionsPerSession = 24098 (ns=0, i=24098)
    // - Server_ServerCapabilities_MaxMonitoredItemsPerSubscription = 24104 (ns=0, i=24104)
    let nodes = [
        ReadValueId::new_value(NodeId::new(0, 24095u32)),
        ReadValueId::new_value(NodeId::new(0, 24096u32)),
        ReadValueId::new_value(NodeId::new(0, 24097u32)),
        ReadValueId::new_value(NodeId::new(0, 24098u32)),
        ReadValueId::new_value(NodeId::new(0, 24104u32)),
    ];
    let mut capacity = SubscriptionCapacity::default();

    match session.read(&nodes, TimestampsToReturn::Neither, 0.0).await {
        Ok(values) => {
            let expected = nodes.len();
            if values.len() != expected {
                tracing::warn!(
                    expected,
                    actual = values.len(),
                    "OPC UA server capabilities read returned mismatched value count"
                );
            }
            let to_u32 = |idx: usize, label: &str| -> Option<u32> {
                match values.get(idx).and_then(|dv| dv.value.as_ref()) {
                    Some(variant) => {
                        let value = match variant {
                            Variant::UInt32(v) => Some(*v),
                            Variant::Int32(v) if *v >= 0 => Some(*v as u32),
                            Variant::UInt64(v) if *v <= u32::MAX as u64 => Some(*v as u32),
                            Variant::Int64(v) if *v >= 0 && *v <= u32::MAX as i64 => {
                                Some(*v as u32)
                            }
                            _ => {
                                tracing::warn!(
                                    label,
                                    variant = ?variant,
                                    "OPC UA server capabilities value has unexpected type"
                                );
                                None
                            }
                        };
                        if value.is_none() {
                            tracing::debug!(
                                label,
                                "OPC UA server capabilities value parsed as None"
                            );
                        }
                        value
                    }
                    None => {
                        tracing::debug!(label, "OPC UA server capabilities value is empty");
                        None
                    }
                }
            };

            capacity.max_sessions = to_u32(0, "MaxSessions");
            capacity.max_subscriptions = to_u32(1, "MaxSubscriptions");
            capacity.max_monitored_items = to_u32(2, "MaxMonitoredItems");
            capacity.max_subscriptions_per_session = to_u32(3, "MaxSubscriptionsPerSession");
            capacity.max_monitored_items_per_subscription =
                to_u32(4, "MaxMonitoredItemsPerSubscription");

            tracing::info!(
                max_sessions = ?capacity.max_sessions,
                max_subscriptions = ?capacity.max_subscriptions,
                max_monitored_items = ?capacity.max_monitored_items,
                max_subscriptions_per_session = ?capacity.max_subscriptions_per_session,
                max_monitored_items_per_subscription =
                    ?capacity.max_monitored_items_per_subscription,
                "OPC UA server subscription capacity read from ServerCapabilities"
            );
        }
        Err(status) => {
            tracing::warn!(
                status = %status,
                "Failed to read OPC UA server subscription capabilities; continuing with default capacity"
            );
        }
    }

    capacity
}

#[inline]
fn make_callbacks(
    publisher: Arc<dyn NorthwardPublisher>,
    snapshot: Arc<ArcSwap<PointSnapshot>>,
) -> SubscriptionCallbacks {
    SubscriptionCallbacks::new(
        |_status| {},
        move |dv: DataValue, item: &MonitoredItem| {
            if dv.status.as_ref().map(|s| s.is_bad()).unwrap_or(false) {
                return;
            }
            let Some(variant) = dv.value.as_ref() else {
                return;
            };
            let node_id_ref = &item.item_to_monitor().node_id;
            let snap = snapshot.load();
            if let Some(meta) = snap.node_to_meta.get(node_id_ref) {
                if let Some(typed) = meta.coerce(variant) {
                    if let Some(dev_meta) = snap.devices.get(&meta.device_id) {
                        if dev_meta.status == Status::Enabled {
                            let device_name = dev_meta.name.clone();
                            match meta.kind() {
                                DataPointType::Telemetry => {
                                    let data = NorthwardData::Telemetry(TelemetryData::new(
                                        meta.device_id,
                                        device_name,
                                        vec![PointValue {
                                            point_id: meta.point.id,
                                            point_key: Arc::<str>::from(meta.point.key.as_str()),
                                            value: typed,
                                        }],
                                    ));
                                    let _ = publisher.try_publish(Arc::new(data));
                                }
                                DataPointType::Attribute => {
                                    let data = NorthwardData::Attributes(
                                        AttributeData::new_client_attributes(
                                            meta.device_id,
                                            device_name,
                                            vec![PointValue {
                                                point_id: meta.point.id,
                                                point_key: Arc::<str>::from(
                                                    meta.point.key.as_str(),
                                                ),
                                                value: typed,
                                            }],
                                        ),
                                    );
                                    let _ = publisher.try_publish(Arc::new(data));
                                }
                            }
                        }
                    } else {
                        tracing::warn!(
                            device_id = meta.device_id,
                            node_id = ?node_id_ref,
                            "Device metadata not found; dropping data"
                        );
                    }
                }
            }
        },
        |_e, _i| {},
    )
}

#[inline]
fn build_reqs(nodes: &[NodeId], next_handle: &mut u32) -> Vec<MonitoredItemCreateRequest> {
    nodes
        .iter()
        .cloned()
        .map(|n| {
            let item_to_monitor = ReadValueId::new_value(n);
            let requested_parameters = MonitoringParameters {
                client_handle: {
                    let h = *next_handle;
                    *next_handle = next_handle.saturating_add(1);
                    h
                },
                sampling_interval: 500.0,
                filter: Default::default(),
                queue_size: 20,
                discard_oldest: true,
            };
            MonitoredItemCreateRequest {
                item_to_monitor,
                monitoring_mode: MonitoringMode::Reporting,
                requested_parameters,
            }
        })
        .collect()
}

#[inline]
async fn register_nodes_chunked(
    session: &Arc<Session>,
    sid: u32,
    nodes: &[NodeId],
    batch_size: usize,
    item_map: &mut HashMap<NodeId, MonitoredItemInfo>,
    next_handle: &mut u32,
) {
    let chunk = batch_size.max(1);
    let mut start = 0usize;
    while start < nodes.len() {
        let end = (start + chunk).min(nodes.len());
        let reqs = build_reqs(&nodes[start..end], next_handle);
        match session
            .create_monitored_items(sid, TimestampsToReturn::Both, reqs)
            .await
        {
            Ok(results) => {
                // Defensive check: result length should match requested nodes length for this chunk.
                if results.len() != (end - start) {
                    tracing::error!(
                        subscription_id = sid,
                        expected = end - start,
                        actual = results.len(),
                        "OPC UA create_monitored_items returned mismatched result length"
                    );
                }

                for (i, res) in results.into_iter().enumerate() {
                    // Guard against out-of-bounds if server returned fewer results.
                    let Some(node) = nodes.get(start + i).cloned() else {
                        tracing::error!(
                            subscription_id = sid,
                            result_index = i,
                            "OPC UA create_monitored_items result index out of range for nodes slice"
                        );
                        break;
                    };

                    let status = res.result.status_code;
                    if !status.is_good() {
                        let failure_kind: MonitoredItemFailureKind = status.into();

                        // Per-item failure is logged with full context but will not be inserted
                        // into the local item_map to keep it consistent with server state.
                        tracing::warn!(
                            subscription_id = sid,
                            ?node,
                            ?status,
                            failure_kind = failure_kind.as_str(),
                            "OPC UA monitored item creation failed for node"
                        );
                        continue;
                    }

                    item_map.insert(
                        node.clone(),
                        MonitoredItemInfo {
                            monitored_item_id: res.result.monitored_item_id,
                            client_handle: res.requested_parameters.client_handle,
                            revised_sampling_ms: res
                                .result
                                .revised_sampling_interval
                                .max(0.0)
                                .floor() as u64,
                            revised_queue_size: res.result.revised_queue_size,
                            node_id: node,
                        },
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    subscription_id = sid,
                    error = %e,
                    "OPC UA create_monitored_items RPC failed for chunk"
                );
            }
        }
        start = end;
    }
}

/// SubscriptionManager owns the command channel and background task that manages
/// the OPC UA subscription and its MonitoredItems according to runtime deltas.
pub(super) struct SubscriptionManager {
    tx: mpsc::Sender<SubscriptionCommand>,
}

impl SubscriptionManager {
    /// Create a new manager and spawn its background loop.
    pub fn new_with_actor(
        cancel: CancellationToken,
        publisher: Arc<dyn NorthwardPublisher>,
        snapshot: Arc<ArcSwap<PointSnapshot>>,
        batch_size: usize,
    ) -> (Self, SubscriptionActor) {
        let (tx, rx) = mpsc::channel::<SubscriptionCommand>(1024);
        let actor = SubscriptionActor {
            rx,
            cancel,
            publisher,
            snapshot,
            batch_size,
            session: None,
            subscriptions: Vec::new(),
            next_handle: 1,
            capacity: SubscriptionCapacity::default(),
        };
        (Self { tx }, actor)
    }

    /// Get a clone of the internal sender to send commands to the manager.
    pub async fn send_command(&self, command: SubscriptionCommand) {
        let _ = self.tx.send(command).await;
    }
}
