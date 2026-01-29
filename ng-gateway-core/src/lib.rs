pub mod collector;
pub mod commands;
pub mod gateway;
pub mod lifecycle;
pub mod northward;
pub mod observability;
pub mod realtime;
pub mod southward;

// Re-export commonly used types
pub use collector::NGCollector;
pub use gateway::NGGateway;
pub use northward::NorthwardEventsBus;
pub use realtime::NGRealtimeMonitorHub;
pub use southward::NGSouthwardManager;
pub use southward::SouthwardDataBus;

// Re-export common lifecycle types for convenience
pub use lifecycle::StartPolicy;
