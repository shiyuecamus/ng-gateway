use crate::{
    codec::value_to_variant,
    config::{DropPolicy, OpcuaServerPluginConfig},
    node_cache::NodeCache,
    node_id::make_nodeid_path,
    queue::{create_update_queue, UpdateBatch, UpdateKind, UpdateQueueRx, UpdateQueueTx},
    server::OpcuaServerRuntime,
    write_dispatch::WriteDispatcher,
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    NorthwardConnectionState, NorthwardData, NorthwardError, NorthwardInitContext, NorthwardResult,
    NorthwardRuntimeApi, Plugin, PointMeta, PointValue, RuntimeDelta,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::info;

pub struct OpcuaServerPlugin {
    config: Arc<OpcuaServerPluginConfig>,
    runtime: Arc<dyn NorthwardRuntimeApi>,
    plugin_id: i32,

    conn_state_tx: watch::Sender<NorthwardConnectionState>,
    conn_state_rx: watch::Receiver<NorthwardConnectionState>,

    node_cache: Arc<NodeCache>,
    /// A small control-plane queue used to lazily create AddressSpace nodes for newly seen points.
    node_build_tx: mpsc::Sender<i32>,
    node_build_rx: std::sync::Mutex<Option<mpsc::Receiver<i32>>>,
    update_tx: UpdateQueueTx,
    update_rx: Arc<tokio::sync::Mutex<UpdateQueueRx>>,

    write_dispatch: Arc<WriteDispatcher>,

    shutdown: CancellationToken,

    // created in start()
    server: tokio::sync::RwLock<Option<OpcuaServerRuntime>>,
}

impl OpcuaServerPlugin {
    pub fn with_ctx(ctx: NorthwardInitContext) -> NorthwardResult<Self> {
        let (conn_state_tx, conn_state_rx) = watch::channel(NorthwardConnectionState::Disconnected);
        let config = ctx
            .config
            .downcast_arc::<OpcuaServerPluginConfig>()
            .map_err(|_| NorthwardError::ConfigurationError {
                message: "Failed to downcast to OpcuaServerPluginConfig".to_string(),
            })?;

        let (update_tx, update_rx) =
            create_update_queue(config.update_queue_capacity, config.drop_policy);

        let (node_build_tx, node_build_rx) = mpsc::channel::<i32>(4096);

        let node_cache = Arc::new(NodeCache::new());
        let write_dispatch = Arc::new(WriteDispatcher::new(
            Arc::clone(&config),
            Arc::clone(&ctx.runtime),
            Arc::clone(&node_cache),
            ctx.events_tx,
        ));

        Ok(Self {
            config,
            runtime: ctx.runtime,
            plugin_id: ctx.app_id,
            conn_state_tx,
            conn_state_rx,
            node_cache,
            node_build_tx,
            node_build_rx: std::sync::Mutex::new(Some(node_build_rx)),
            update_tx,
            update_rx: Arc::new(tokio::sync::Mutex::new(update_rx)),
            write_dispatch,
            shutdown: CancellationToken::new(),
            server: tokio::sync::RwLock::new(None),
        })
    }

    fn make_full_node_id(namespace_index: u16, meta: &PointMeta) -> String {
        // NodeId rule: ns={namespace_index};s={channel}.{device}.{point}
        let path = make_nodeid_path(
            meta.channel_name.as_ref(),
            meta.device_name.as_ref(),
            meta.point_key.as_ref(),
        );
        format!("ns={namespace_index};s={path}")
    }

    fn schedule_point_nodes(&self, values: &[PointValue]) {
        for pv in values {
            if self.node_cache.get_node_id(pv.point_id).is_some() {
                continue;
            }
            // Best-effort: if the queue is full, we prefer dropping control-plane work
            // over blocking telemetry hot path.
            let _ = self.node_build_tx.try_send(pv.point_id);
        }
    }

    async fn run_node_builder(
        runtime: Arc<dyn NorthwardRuntimeApi>,
        node_cache: Arc<NodeCache>,
        shutdown: CancellationToken,
        server: OpcuaServerRuntime,
        mut rx: mpsc::Receiver<i32>,
        namespace_index: u16,
    ) {
        while let Some(point_id) = tokio::select! {
            _ = shutdown.cancelled() => None,
            v = rx.recv() => v,
        } {
            if node_cache.get_node_id(point_id).is_some() {
                continue;
            }
            let Some(meta) = runtime.get_point_meta(point_id) else {
                continue;
            };
            let node_id = Self::make_full_node_id(namespace_index, meta.as_ref());
            node_cache.upsert(meta.point_id, Arc::<str>::from(node_id.as_str()));
            server.upsert_point_node(meta.as_ref(), &node_id);
        }
    }

    async fn run_delta_listener(
        runtime: Arc<dyn NorthwardRuntimeApi>,
        node_cache: Arc<NodeCache>,
        shutdown: CancellationToken,
        server: OpcuaServerRuntime,
        namespace_index: u16,
    ) {
        let mut rx = runtime.subscribe_runtime_delta();
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                msg = rx.recv() => {
                    let delta = match msg {
                        Ok(d) => d,
                        Err(_) => continue,
                    };
                    match delta {
                        RuntimeDelta::PointsChanged { added, updated, removed, .. } => {
                            // IMPORTANT:
                            // `RuntimeDelta` is a global stream (not scoped to app subscription).
                            // To avoid exposing un-subscribed devices/points, we only update nodes
                            // that already exist in our local cache (i.e., were seen via routed data).
                            for p in added.iter().chain(updated.iter()) {
                                if node_cache.get_node_id(p.id()).is_none() {
                                    continue;
                                }
                                if let Some(meta) = runtime.get_point_meta(p.id()) {
                                    let node_id = Self::make_full_node_id(namespace_index, meta.as_ref());
                                    node_cache.upsert(meta.point_id, Arc::<str>::from(node_id.as_str()));
                                    server.upsert_point_node(meta.as_ref(), &node_id);
                                }
                            }
                            for p in removed.iter() {
                                if let Some(node_id) = node_cache.remove_by_point(p.id()) {
                                    server.remove_node(node_id.as_ref());
                                }
                            }
                        }
                        RuntimeDelta::DevicesChanged { .. } => {
                            // no-op: point meta is sufficient to build hierarchy
                        }
                        RuntimeDelta::ActionsChanged { .. } => {
                            // no-op for now (write-back uses action_key template)
                        }
                    }
                }
            }
        }
    }

    async fn run_applier(
        runtime: Arc<dyn NorthwardRuntimeApi>,
        node_cache: Arc<NodeCache>,
        update_rx: Arc<tokio::sync::Mutex<UpdateQueueRx>>,
        shutdown: CancellationToken,
        server: OpcuaServerRuntime,
        namespace_index: u16,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                batch = async {
                    let mut rx = update_rx.lock().await;
                    rx.recv().await
                } => {
                    let Some(batch) = batch else { break };

                    for pv in batch.values.iter() {
                        // Fix "first value is empty" race:
                        // the first batch may arrive before the async node builder has created
                        // the AddressSpace node. If so, create it here (cold path per point).
                        let node_id = match node_cache.get_node_id(pv.point_id) {
                            Some(id) => id,
                            None => {
                                let Some(meta) = runtime.get_point_meta(pv.point_id) else {
                                    continue;
                                };
                                let full = Self::make_full_node_id(namespace_index, meta.as_ref());
                                let arc = Arc::<str>::from(full.as_str());
                                node_cache.upsert(meta.point_id, Arc::clone(&arc));
                                server.upsert_point_node(meta.as_ref(), &full);
                                arc
                            }
                        };
                        server.set_value(node_id.as_ref(), value_to_variant(&pv.value));
                    }
                }
            }
        }
    }

    async fn enqueue_batch(&self, batch: UpdateBatch) -> NorthwardResult<()> {
        match self.config.drop_policy {
            DropPolicy::BlockWithTimeout => {
                // Best-effort bounded wait; still keep this short to avoid blocking collection.
                let tx = self.update_tx.clone();
                let _ = tokio::time::timeout(Duration::from_millis(50), tx.enqueue_blocking(batch))
                    .await
                    .map_err(|_| NorthwardError::QueueFull)?;
                Ok(())
            }
            _ => self
                .update_tx
                .try_enqueue(batch)
                .map_err(|_| NorthwardError::QueueFull),
        }
    }
}

#[async_trait]
impl Plugin for OpcuaServerPlugin {
    async fn start(&self) -> NorthwardResult<()> {
        info!("Starting OPC UA Server plugin");

        let write_dispatch = Arc::clone(&self.write_dispatch);

        let server = OpcuaServerRuntime::start(
            self.plugin_id,
            Arc::clone(&self.config),
            Arc::clone(&self.runtime),
            Arc::clone(&self.node_cache),
            write_dispatch,
            self.conn_state_tx.clone(),
            self.shutdown.child_token(),
        )
        .await?;

        let namespace_index = server.namespace_index();

        // Save server handle
        *self.server.write().await = Some(server.clone());

        // Spawn node builder + applier + delta listener
        let Some(node_build_rx) = self.node_build_rx.lock().unwrap().take() else {
            return Err(NorthwardError::ConfigurationError {
                message: "node builder already started".to_string(),
            });
        };
        let runtime = Arc::clone(&self.runtime);
        let node_cache = Arc::clone(&self.node_cache);
        let shutdown = self.shutdown.child_token();
        let server_for_builder = server.clone();
        let ns_for_builder = namespace_index;
        tokio::spawn(async move {
            Self::run_node_builder(
                runtime,
                node_cache,
                shutdown,
                server_for_builder,
                node_build_rx,
                ns_for_builder,
            )
            .await
        });

        // applier + delta listener
        let runtime = Arc::clone(&self.runtime);
        let node_cache = Arc::clone(&self.node_cache);
        let shutdown = self.shutdown.child_token();
        let server_for_delta = server.clone();
        let ns_for_delta = namespace_index;
        tokio::spawn(async move {
            Self::run_delta_listener(
                runtime,
                node_cache,
                shutdown,
                server_for_delta,
                ns_for_delta,
            )
            .await
        });

        let runtime = Arc::clone(&self.runtime);
        let node_cache = Arc::clone(&self.node_cache);
        let update_rx = Arc::clone(&self.update_rx);
        let shutdown = self.shutdown.child_token();
        let server_for_applier = server.clone();
        let ns_for_applier = namespace_index;
        tokio::spawn(async move {
            Self::run_applier(
                runtime,
                node_cache,
                update_rx,
                shutdown,
                server_for_applier,
                ns_for_applier,
            )
            .await
        });

        info!("OPC UA Server plugin started");
        Ok(())
    }

    fn subscribe_connection_state(&self) -> watch::Receiver<NorthwardConnectionState> {
        self.conn_state_rx.clone()
    }

    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        match data.as_ref() {
            NorthwardData::Telemetry(t) => {
                self.schedule_point_nodes(&t.values);
                let batch = UpdateBatch {
                    timestamp: t.timestamp,
                    kind: UpdateKind::Telemetry,
                    values: Arc::from(t.values.clone().into_boxed_slice()),
                };
                self.enqueue_batch(batch).await?;
            }
            NorthwardData::Attributes(a) => {
                // Merge all attribute scopes for OPC UA exposure
                let mut all = Vec::with_capacity(
                    a.client_attributes.len()
                        + a.shared_attributes.len()
                        + a.server_attributes.len(),
                );
                all.extend_from_slice(&a.client_attributes);
                all.extend_from_slice(&a.shared_attributes);
                all.extend_from_slice(&a.server_attributes);
                self.schedule_point_nodes(&all);
                let batch = UpdateBatch {
                    timestamp: a.timestamp,
                    kind: UpdateKind::Attributes,
                    values: Arc::from(all.into_boxed_slice()),
                };
                self.enqueue_batch(batch).await?;
            }
            NorthwardData::WritePointResponse(resp) => {
                self.write_dispatch
                    .on_write_point_response(resp.clone())
                    .await;
            }
            _ => {
                // ignore (connection events, rpc responses, alarms)
            }
        }
        Ok(())
    }

    async fn stop(&self) -> NorthwardResult<()> {
        info!("Stopping OPC UA Server plugin");
        self.shutdown.cancel();
        let _ = self
            .conn_state_tx
            .send(NorthwardConnectionState::Disconnected);
        *self.server.write().await = None;
        Ok(())
    }
}
