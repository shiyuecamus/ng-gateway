//! OPC UA Server data-plane handle implementation.
//!
//! `OpcuaServerHandle` is published by the SDK supervisor when the plugin attempt is Ready.
//! It consumes northward `NorthwardData` and enqueues updates into an internal queue so
//! the server-side applier task can update AddressSpace efficiently.

use super::{
    config::{DropPolicy, OpcuaServerPluginConfig},
    node_cache::NodeCache,
    queue::{UpdateBatch, UpdateKind, UpdateQueueTx},
    write_dispatch::WriteDispatcher,
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    NorthwardData, NorthwardError, NorthwardHandle, NorthwardResult, NorthwardRuntimeApi,
    PointValue,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

/// OPC UA Server data-plane handle.
pub struct OpcuaServerHandle {
    pub(crate) config: Arc<OpcuaServerPluginConfig>,
    pub(crate) runtime: Arc<dyn NorthwardRuntimeApi>,
    pub(crate) node_cache: Arc<NodeCache>,
    pub(crate) node_build_tx: mpsc::Sender<i32>,
    pub(crate) update_tx: UpdateQueueTx,
    pub(crate) write_dispatch: Arc<WriteDispatcher>,
}

impl OpcuaServerHandle {
    pub fn new(
        config: Arc<OpcuaServerPluginConfig>,
        runtime: Arc<dyn NorthwardRuntimeApi>,
        node_cache: Arc<NodeCache>,
        node_build_tx: mpsc::Sender<i32>,
        update_tx: UpdateQueueTx,
        write_dispatch: Arc<WriteDispatcher>,
    ) -> Self {
        Self {
            config,
            runtime,
            node_cache,
            node_build_tx,
            update_tx,
            write_dispatch,
        }
    }

    pub(crate) fn schedule_point_nodes(&self, values: &[PointValue]) {
        for pv in values {
            if self.node_cache.get_node_id(pv.point_id).is_some() {
                continue;
            }
            // Best-effort: prefer dropping control-plane work over blocking telemetry hot path.
            let _ = self.node_build_tx.try_send(pv.point_id);
        }
    }

    pub(crate) async fn enqueue_batch(&self, batch: UpdateBatch) -> NorthwardResult<()> {
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
impl NorthwardHandle for OpcuaServerHandle {
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
                // ignore
            }
        }
        Ok(())
    }
}
