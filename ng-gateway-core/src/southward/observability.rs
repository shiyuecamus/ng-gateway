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
    fn add_bytes_in(&self, bytes: u64) {
        self.prom.add_bytes_received(bytes);
    }

    #[inline]
    fn add_bytes_out(&self, bytes: u64) {
        self.prom.add_bytes_sent(bytes);
    }
}
