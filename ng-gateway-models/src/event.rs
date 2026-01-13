use downcast_rs::{impl_downcast, DowncastSync};
use ng_gateway_macros::Event;
use opentelemetry::{
    global,
    metrics::{Counter, UpDownCounter},
};

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
    pub enable_metrics: bool,
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

    pub fn set_enable_metrics(&mut self, enable_metrics: bool) -> &mut Self {
        self.enable_metrics = enable_metrics;
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
            enable_metrics: true,
            enable_tracing: true,
        }
    }
}

/// OpenTelemetry metrics for event bus
#[derive(Debug, Clone)]
pub struct EventBusMetrics {
    pub total_events: Counter<u64>,
    pub successful_handlers: Counter<u64>,
    pub failed_handlers: Counter<u64>,
    pub active_handlers: UpDownCounter<i64>,
}

impl EventBusMetrics {
    pub fn new() -> Self {
        let meter = global::meter("event_bus");
        Self {
            total_events: meter
                .u64_counter("event_bus.total_events")
                .with_description("Total number of events processed")
                .build(),
            successful_handlers: meter
                .u64_counter("event_bus.successful_handlers")
                .with_description("Number of successfully processed events")
                .build(),
            failed_handlers: meter
                .u64_counter("event_bus.failed_handlers")
                .with_description("Number of failed event handlers")
                .build(),
            active_handlers: meter
                .i64_up_down_counter("event_bus.active_handlers")
                .with_description("Number of active event handlers")
                .build(),
        }
    }
}

impl Default for EventBusMetrics {
    fn default() -> Self {
        Self::new()
    }
}
