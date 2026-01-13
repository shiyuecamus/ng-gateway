use crate::southward::manager::NGSouthwardManager;
use async_trait::async_trait;
use ng_gateway_sdk::{NorthwardRuntimeApi, PointMeta, RuntimeDelta};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Core implementation of `NorthwardRuntimeApi`.
///
/// This adapter exposes a read-only view of gateway runtime indexes to northward plugins.
/// All lookups are backed by `DashMap` and are intended to be O(1).
#[derive(Clone)]
pub struct CoreNorthwardRuntimeApi {
    southward: Arc<NGSouthwardManager>,
}

impl CoreNorthwardRuntimeApi {
    /// Create a new runtime API backed by the given southward manager.
    pub fn new(southward: Arc<NGSouthwardManager>) -> Self {
        Self { southward }
    }

    #[inline]
    fn make_path_key(channel_name: &str, device_name: &str, point_key: &str) -> String {
        // NOTE: This is not a hot path. Prefer `get_point_meta(point_id)` for telemetry encoding.
        // We use a unit separator character to reduce collision risks.
        const SEP: char = '\u{1f}';
        let mut s =
            String::with_capacity(channel_name.len() + device_name.len() + point_key.len() + 2);
        s.push_str(channel_name);
        s.push(SEP);
        s.push_str(device_name);
        s.push(SEP);
        s.push_str(point_key);
        s
    }
}

#[async_trait]
impl NorthwardRuntimeApi for CoreNorthwardRuntimeApi {
    #[inline]
    fn get_point_meta(&self, point_id: i32) -> Option<Arc<PointMeta>> {
        let index = self.southward.runtime_index();
        index
            .point_entries_by_id
            .get(&point_id)
            .map(|e| Arc::clone(&e.value().meta))
    }

    #[inline]
    fn get_point_meta_by_path(
        &self,
        channel_name: &str,
        device_name: &str,
        point_key: &str,
    ) -> Option<Arc<PointMeta>> {
        let index = self.southward.runtime_index();
        let key = Self::make_path_key(channel_name, device_name, point_key);
        let point_id = index.point_id_by_path.get(&key).map(|e| *e.value())?;
        index
            .point_entries_by_id
            .get(&point_id)
            .map(|e| Arc::clone(&e.value().meta))
    }

    #[inline]
    fn subscribe_runtime_delta(&self) -> broadcast::Receiver<RuntimeDelta> {
        let index = self.southward.runtime_index();
        index.runtime_delta_tx.subscribe()
    }

    #[inline]
    fn list_point_meta(&self) -> Vec<Arc<PointMeta>> {
        let index = self.southward.runtime_index();
        index
            .point_entries_by_id
            .iter()
            .map(|e| Arc::clone(&e.value().meta))
            .collect()
    }
}
