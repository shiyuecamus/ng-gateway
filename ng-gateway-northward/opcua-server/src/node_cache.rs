use dashmap::DashMap;
use std::sync::Arc;

/// Bi-directional cache:
/// - `point_id -> node_id_string`
/// - `node_id_string -> point_id`
///
/// For OPC UA write-back callbacks we need reverse lookup.
#[derive(Debug, Default)]
pub struct NodeCache {
    by_point: DashMap<i32, Arc<str>>,
    by_node: DashMap<Arc<str>, i32>,
}

impl NodeCache {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn get_node_id(&self, point_id: i32) -> Option<Arc<str>> {
        self.by_point.get(&point_id).map(|e| Arc::clone(e.value()))
    }

    #[inline]
    pub fn get_point_id(&self, node_id: &str) -> Option<i32> {
        self.by_node.get(node_id).map(|e| *e.value())
    }

    pub fn upsert(&self, point_id: i32, node_id: Arc<str>) {
        // Remove old reverse mapping if point_id existed with different node_id
        if let Some(old) = self.by_point.insert(point_id, Arc::clone(&node_id)) {
            self.by_node.remove(old.as_ref());
        }
        self.by_node.insert(node_id, point_id);
    }

    pub fn remove_by_point(&self, point_id: i32) -> Option<Arc<str>> {
        let old = self.by_point.remove(&point_id).map(|(_, v)| v)?;
        self.by_node.remove(old.as_ref());
        Some(old)
    }
}
