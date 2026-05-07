//! Bi-directional `point_id ↔ NodeId` cache shared across supervision attempts.
//!
//! The OPC UA Server plugin needs O(1) lookups in both directions:
//! - `point_id → NodeId` on every value publication / dynamic node build
//! - `NodeId → point_id` on every OPC UA Write callback (reverse lookup)
//!
//! Storing typed `NodeId` (not `Arc<str>`) keeps the hot path allocation-free:
//! `NodeId` already implements `Hash + Eq + Clone`, so it can drive a
//! `DashMap` directly. Compared with the previous `Arc<str>` design we save:
//! - the `format!("ns=N;s=...")` allocation when materialising,
//! - the `NodeId::from_str()` parse on the write callback,
//! - the `node_id.to_string()` allocation on every reverse lookup.

use dashmap::DashMap;
use opcua::types::NodeId;

/// Bi-directional cache: `point_id ↔ NodeId`.
#[derive(Debug, Default)]
pub struct NodeCache {
    by_point: DashMap<i32, NodeId>,
    by_node: DashMap<NodeId, i32>,
}

impl NodeCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the materialised `NodeId` for a gateway point id.
    #[inline]
    pub fn get_node_id(&self, point_id: i32) -> Option<NodeId> {
        self.by_point.get(&point_id).map(|e| e.value().clone())
    }

    /// Reverse lookup: which gateway point owns this `NodeId`?
    #[inline]
    pub fn get_point_id(&self, node_id: &NodeId) -> Option<i32> {
        self.by_node.get(node_id).map(|e| *e.value())
    }

    /// Insert / replace a binding, keeping both directions consistent.
    pub fn upsert(&self, point_id: i32, node_id: NodeId) {
        // Remove old reverse mapping if point_id existed with a different NodeId.
        if let Some(old) = self.by_point.insert(point_id, node_id.clone()) {
            if old != node_id {
                self.by_node.remove(&old);
            }
        }
        self.by_node.insert(node_id, point_id);
    }

    /// Remove the binding owned by `point_id` and return the previously-bound NodeId, if any.
    pub fn remove_by_point(&self, point_id: i32) -> Option<NodeId> {
        let old = self.by_point.remove(&point_id).map(|(_, v)| v)?;
        self.by_node.remove(&old);
        Some(old)
    }

    /// Return a deterministic snapshot of all materialised point bindings.
    ///
    /// # Notes
    /// The snapshot is sorted by NodeId string representation so control-plane
    /// exports produce stable output even though the cache itself is a
    /// concurrent map.
    pub fn snapshot_materialized(&self) -> Vec<(i32, NodeId)> {
        let mut out: Vec<_> = self
            .by_point
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect();
        out.sort_by_key(|a| a.1.to_string());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: u16, s: &str) -> NodeId {
        NodeId::new(id, s.to_string())
    }

    #[test]
    fn upsert_keeps_both_directions_in_sync() {
        let c = NodeCache::new();
        c.upsert(1, n(2, "ch/dev/a"));
        c.upsert(2, n(2, "ch/dev/b"));
        assert_eq!(c.get_node_id(1), Some(n(2, "ch/dev/a")));
        assert_eq!(c.get_point_id(&n(2, "ch/dev/b")), Some(2));
    }

    #[test]
    fn upsert_replaces_old_reverse_mapping() {
        let c = NodeCache::new();
        c.upsert(1, n(2, "ch/dev/a"));
        c.upsert(1, n(2, "ch/dev/renamed"));
        assert_eq!(c.get_node_id(1), Some(n(2, "ch/dev/renamed")));
        // Old reverse mapping must be gone.
        assert_eq!(c.get_point_id(&n(2, "ch/dev/a")), None);
        assert_eq!(c.get_point_id(&n(2, "ch/dev/renamed")), Some(1));
    }

    #[test]
    fn remove_by_point_clears_reverse() {
        let c = NodeCache::new();
        c.upsert(1, n(2, "ch/dev/a"));
        let removed = c.remove_by_point(1);
        assert_eq!(removed, Some(n(2, "ch/dev/a")));
        assert!(c.get_node_id(1).is_none());
        assert!(c.get_point_id(&n(2, "ch/dev/a")).is_none());
    }

    #[test]
    fn snapshot_is_sorted_by_node_id_string() {
        let c = NodeCache::new();
        c.upsert(2, n(1, "z"));
        c.upsert(1, n(1, "a"));
        c.upsert(3, n(1, "m"));
        let snap = c.snapshot_materialized();
        let ids: Vec<i32> = snap.iter().map(|(p, _)| *p).collect();
        // 'a' < 'm' < 'z'
        assert_eq!(ids, vec![1, 3, 2]);
    }

    #[test]
    fn snapshot_handles_chinese_node_ids() {
        let c = NodeCache::new();
        c.upsert(1, n(2, "通道一/1号温湿度计/湿度"));
        let snap = c.snapshot_materialized();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].1.to_string(), "ns=2;s=通道一/1号温湿度计/湿度");
    }
}
