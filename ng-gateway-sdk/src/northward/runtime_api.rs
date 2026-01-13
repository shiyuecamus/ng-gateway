use crate::{PointMeta, RuntimeDelta};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Read-only runtime API for northward plugins.
///
/// # Goals
/// - Keep plugins decoupled from gateway core internal structures
/// - Provide O(1) metadata access on hot paths (telemetry/attributes encoding)
/// - Provide a stable extension surface for future evolution
///
/// # Performance notes
/// - Prefer returning `Arc<PointMeta>` so clones are cheap and avoid per-call allocations.
/// - Implementations should aim for lock-free reads or low-contention reads.
#[async_trait]
pub trait NorthwardRuntimeApi: Send + Sync {
    /// Get point metadata by `point_id` with O(1) complexity.
    fn get_point_meta(&self, point_id: i32) -> Option<Arc<PointMeta>>;

    /// Reverse lookup by `(channel_name, device_name, point_key)`.
    ///
    /// This is primarily used by write-back callbacks and topic/route based protocols.
    /// Implementations should aim for O(1) complexity as well.
    fn get_point_meta_by_path(
        &self,
        channel_name: &str,
        device_name: &str,
        point_key: &str,
    ) -> Option<Arc<PointMeta>>;

    /// Subscribe to runtime topology deltas.
    ///
    /// Plugins can use this stream to keep local caches fresh without polling.
    fn subscribe_runtime_delta(&self) -> broadcast::Receiver<RuntimeDelta>;

    /// List all known point metadata snapshots.
    ///
    /// This is a non-hot-path API intended for plugins that must build an initial
    /// topology (e.g., OPC UA AddressSpace). Implementations should prefer a
    /// zero-copy approach and avoid DB queries.
    fn list_point_meta(&self) -> Vec<Arc<PointMeta>>;
}
