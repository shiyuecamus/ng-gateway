use ng_gateway_common::metrics::southward::SouthwardChannelMetricHandles;
use ng_gateway_sdk::SouthwardTransportMeter;
use std::sync::Arc;

/// Per-channel bound transport meter (hot-path friendly).
///
/// This avoids any map lookups or string allocations on I/O hot paths by binding the
/// meter to pre-resolved channel metric handles.
#[derive(Debug)]
pub struct ChannelBoundTransportMeter {
    prom: Arc<SouthwardChannelMetricHandles>,
}

impl ChannelBoundTransportMeter {
    #[inline]
    pub fn new(prom: Arc<SouthwardChannelMetricHandles>) -> Self {
        Self { prom }
    }
}

impl SouthwardTransportMeter for ChannelBoundTransportMeter {
    #[inline]
    fn add_bytes_in(&self, _channel_id: i32, _driver: &str, _device_id: Option<i32>, bytes: u64) {
        self.prom.add_bytes_received(_device_id, bytes);
    }

    #[inline]
    fn add_bytes_out(&self, _channel_id: i32, _driver: &str, _device_id: Option<i32>, bytes: u64) {
        self.prom.add_bytes_sent(_device_id, bytes);
    }
}
