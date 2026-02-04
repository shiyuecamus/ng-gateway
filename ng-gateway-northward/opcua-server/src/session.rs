//! OPC UA Server supervised session implementation.
//!
//! This module contains the per-attempt session lifecycle driven by the SDK supervisor.
//! - `init()`: spawns background tasks (node builder, runtime delta listener, applier).
//! - `run()`: waits until cancellation.

use super::{
    codec::value_to_variant,
    handle::OpcuaServerHandle,
    node_cache::NodeCache,
    node_id::make_full_node_id,
    queue::{UpdateBatch, UpdateQueueRx},
    server::OpcuaServerRuntime,
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    log::fields as log_fields,
    supervision::{RunOutcome, Session, SessionContext},
    NorthwardError, NorthwardRuntimeApi, RuntimeDelta,
};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{info, Instrument};

/// OPC UA Server session for a single supervision attempt.
pub struct OpcuaServerSession {
    handle: Arc<OpcuaServerHandle>,
    server: OpcuaServerRuntime,
    node_build_rx: Option<mpsc::Receiver<i32>>,
    update_rx: Arc<Mutex<UpdateQueueRx>>,
}

impl OpcuaServerSession {
    pub fn new(
        handle: Arc<OpcuaServerHandle>,
        server: OpcuaServerRuntime,
        node_build_rx: mpsc::Receiver<i32>,
        update_rx: UpdateQueueRx,
    ) -> Self {
        Self {
            handle,
            server,
            node_build_rx: Some(node_build_rx),
            update_rx: Arc::new(Mutex::new(update_rx)),
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
            let node_id = make_full_node_id(namespace_index, meta.as_ref());
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
                            for p in added.iter().chain(updated.iter()) {
                                if node_cache.get_node_id(p.id()).is_none() {
                                    continue;
                                }
                                if let Some(meta) = runtime.get_point_meta(p.id()) {
                                    let node_id = make_full_node_id(namespace_index, meta.as_ref());
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
                        RuntimeDelta::DevicesChanged { .. } => {}
                        RuntimeDelta::ActionsChanged { .. } => {}
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
                    apply_batch(&runtime, &node_cache, &server, namespace_index, &batch).await;
                }
            }
        }
    }
}

async fn apply_batch(
    runtime: &Arc<dyn NorthwardRuntimeApi>,
    node_cache: &Arc<NodeCache>,
    server: &OpcuaServerRuntime,
    namespace_index: u16,
    batch: &UpdateBatch,
) {
    for pv in batch.values.iter() {
        let node_id = match node_cache.get_node_id(pv.point_id) {
            Some(id) => id,
            None => {
                let Some(meta) = runtime.get_point_meta(pv.point_id) else {
                    continue;
                };
                let full = make_full_node_id(namespace_index, meta.as_ref());
                let arc = Arc::<str>::from(full.as_str());
                node_cache.upsert(meta.point_id, Arc::clone(&arc));
                server.upsert_point_node(meta.as_ref(), &full);
                arc
            }
        };
        server.set_value(node_id.as_ref(), value_to_variant(&pv.value));
    }
}

#[async_trait]
impl Session for OpcuaServerSession {
    type Handle = OpcuaServerHandle;
    type Error = NorthwardError;

    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        let _enter = ctx.span.enter();
        let t0 = Instant::now();
        let namespace_index = self.server.namespace_index();

        let Some(node_build_rx) = self.node_build_rx.take() else {
            return Err(NorthwardError::ConfigurationError {
                message: "node builder already started".to_string(),
            });
        };

        let runtime = Arc::clone(&self.handle.runtime);
        let node_cache = Arc::clone(&self.handle.node_cache);
        let shutdown = ctx.cancel.child_token();
        let server_for_builder = self.server.clone();
        tokio::spawn(
            async move {
                Self::run_node_builder(
                    runtime,
                    node_cache,
                    shutdown,
                    server_for_builder,
                    node_build_rx,
                    namespace_index,
                )
                .await
            }
            .in_current_span(),
        );

        let runtime = Arc::clone(&self.handle.runtime);
        let node_cache = Arc::clone(&self.handle.node_cache);
        let shutdown = ctx.cancel.child_token();
        let server_for_delta = self.server.clone();
        tokio::spawn(
            async move {
                Self::run_delta_listener(
                    runtime,
                    node_cache,
                    shutdown,
                    server_for_delta,
                    namespace_index,
                )
                .await
            }
            .in_current_span(),
        );

        let runtime = Arc::clone(&self.handle.runtime);
        let node_cache = Arc::clone(&self.handle.node_cache);
        let update_rx = Arc::clone(&self.update_rx);
        let shutdown = ctx.cancel.child_token();
        let server_for_applier = self.server.clone();
        tokio::spawn(
            async move {
                Self::run_applier(
                    runtime,
                    node_cache,
                    update_rx,
                    shutdown,
                    server_for_applier,
                    namespace_index,
                )
                .await
            }
            .in_current_span(),
        );

        info!(
            target: log_fields::TARGET_PLUGIN,
            attempt = ctx.attempt,
            namespace_index = namespace_index,
            init_ms = t0.elapsed().as_millis() as u64,
            "opcua-server init: background tasks spawned"
        );
        Ok(())
    }

    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        ctx.cancel.cancelled().await;
        Ok(RunOutcome::Disconnected)
    }
}
