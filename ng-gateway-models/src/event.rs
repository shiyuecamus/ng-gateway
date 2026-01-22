use downcast_rs::{impl_downcast, DowncastSync};
use ng_gateway_macros::Event;

impl_downcast!(sync NGEvent);

#[derive(Debug, Clone, Default, Event)]
pub struct ApplicationReady;

#[derive(Debug, Clone, Default, Event)]
pub struct ApplicationShutdown;

#[derive(Debug, Clone, Default, Event)]
pub struct TransportApiAlready;

/// Trait that all events must implement
pub trait NGEvent: DowncastSync + Send + Sync + 'static {}

/// Event statistics for monitoring and observability
#[derive(Debug, Default, Clone)]
pub struct EventStats {
    pub total_events: u64,
    pub successful_handlers: u64,
    pub failed_handlers: u64,
}

/// Event bus configuration
#[derive(Debug, Clone)]
pub struct EventBusConfig {
    pub channel_capacity: usize,
    pub enable_tracing: bool,
}

impl EventBusConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_channel_capacity(&mut self, capacity: usize) -> &mut Self {
        self.channel_capacity = capacity;
        self
    }

    pub fn set_enable_tracing(&mut self, enable_tracing: bool) -> &mut Self {
        self.enable_tracing = enable_tracing;
        self
    }
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 1024,
            enable_tracing: true,
        }
    }
}
