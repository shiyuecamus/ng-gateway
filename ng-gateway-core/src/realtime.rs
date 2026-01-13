//! Realtime device data monitor hub at gateway level.
//!
//! This hub exposes a lightweight pub-sub API for real-time monitoring
//! consumers (e.g. WebSocket clients) to subscribe to `NorthwardData`
//! events for specific devices without interfering with northward app
//! routing.

use dashmap::DashMap;
use ng_gateway_models::RealtimeMonitorHub;
use ng_gateway_sdk::NorthwardData;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Realtime monitor hub for device-level northward data broadcasting.
///
/// Each device id has a dedicated `broadcast::Sender` that fan-outs
/// raw `NorthwardData` instances to all active subscribers.
#[derive(Clone)]
pub struct NGRealtimeMonitorHub {
    /// Per-device broadcast channels.
    device_channels: Arc<DashMap<i32, broadcast::Sender<Arc<NorthwardData>>>>,
}

impl Default for NGRealtimeMonitorHub {
    fn default() -> Self {
        Self::new()
    }
}

impl NGRealtimeMonitorHub {
    /// Create a new, empty realtime monitor hub.
    pub fn new() -> Self {
        Self {
            device_channels: Arc::new(DashMap::new()),
        }
    }
}

impl RealtimeMonitorHub for NGRealtimeMonitorHub {
    /// Subscribe to realtime data for a specific device.
    ///
    /// This returns a `broadcast::Receiver` that will receive all future
    /// `NorthwardData` updates for the given device id. If the device channel
    /// does not exist yet, it will be created lazily.
    fn subscribe(&self, device_id: i32) -> broadcast::Receiver<Arc<NorthwardData>> {
        let sender = self
            .device_channels
            .entry(device_id)
            .or_insert_with(|| {
                // Use a reasonably sized buffer to balance burst handling and memory.
                let (tx, _rx) = broadcast::channel(1024);
                tx
            })
            .value()
            .clone();

        sender.subscribe()
    }

    /// Broadcast a single `NorthwardData` event to all subscribers of the device.
    ///
    /// This is a best-effort operation – lagging subscribers are skipped via
    /// `broadcast`'s built-in backpressure semantics.
    fn broadcast(&self, data: &Arc<NorthwardData>) {
        let device_id = data.device_id();
        if let Some(entry) = self.device_channels.get(&device_id) {
            // Ignore send errors (e.g. no active receivers or lagged receivers).
            let _ = entry.send(Arc::clone(data));
        }
    }
}
