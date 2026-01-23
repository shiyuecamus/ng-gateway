use crate::enums::core::GatewayState;
use chrono::{DateTime, Duration, Utc};
use ng_gateway_sdk::{DeviceState, HealthStatus, SouthwardConnectionState};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

/// Gateway-level aggregated metrics snapshot.
///
/// This struct is a **snapshot DTO** only and MUST NOT contain runtime state
/// (Atomics, Locks, DashMap, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayMetricsSnapshot {
    /// System uptime.
    pub uptime: Duration,

    /// Channel metrics.
    pub total_channels: usize,
    pub connected_channels: usize,

    /// Device metrics.
    pub total_devices: usize,
    pub active_devices: usize,
    pub total_data_points: usize,

    /// Collection metrics.
    pub total_collections: u64,
    pub successful_collections: u64,
    pub failed_collections: u64,
    pub timeout_collections: u64,
    /// Average collection time in milliseconds.
    pub average_collection_time_ms: f64,
    /// Number of active collection tasks.
    pub active_tasks: usize,

    /// Performance metrics.
    pub memory_usage: u64,
    pub cpu_usage: f64,
    pub network_bytes_sent: u64,
    pub network_bytes_received: u64,

    /// Error metrics.
    pub total_errors: u64,
    /// Error rate (errors per minute).
    pub error_rate: f64,

    /// Last update timestamp.
    pub last_update: Option<DateTime<Utc>>,
}

/// Gateway status snapshot (fully serializable).
///
/// This is the only gateway status type exposed to REST/WS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayStatusSnapshot {
    /// Gateway running state.
    pub state: GatewayState,
    /// Gateway-level aggregated metrics snapshot.
    pub metrics: GatewayMetricsSnapshot,
    /// Southward (data collection) subsystem metrics snapshot.
    pub southward_metrics: SouthwardManagerMetricsSnapshot,
    /// Northward (data forwarding) subsystem metrics snapshot.
    pub northward_metrics: NorthwardManagerMetricsSnapshot,
    /// Collection engine metrics snapshot.
    pub collector_metrics: CollectorMetricsSnapshot,
    /// Gateway version.
    pub version: String,
    /// System information snapshot.
    pub system_info: SystemInfoSnapshot,
}

/// System information snapshot (real-time at the moment of snapshot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfoSnapshot {
    /// Operating system type (e.g., "Linux", "Windows", "macOS").
    pub os_type: String,
    /// Operating system architecture (e.g., "x86_64", "aarch64").
    pub os_arch: String,
    /// Hostname.
    pub hostname: Option<String>,
    /// Number of CPU cores.
    pub cpu_cores: usize,
    /// Total memory in bytes.
    pub total_memory: u64,
    /// Used memory in bytes.
    pub used_memory: u64,
    /// Memory usage percentage (0-100).
    pub memory_usage_percent: f64,
    /// CPU usage percentage (0-100).
    pub cpu_usage_percent: f64,
    /// Total disk space in bytes.
    pub total_disk: u64,
    /// Used disk space in bytes.
    pub used_disk: u64,
    /// Disk usage percentage (0-100).
    pub disk_usage_percent: f64,
}

/// Device performance metrics snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceMetricsSnapshot {
    pub total_collections: u64,
    pub successful_collections: u64,
    pub failed_collections: u64,
    pub data_points_collected: u64,
    pub average_collection_time: Duration,
    pub last_collection_time: Duration,
    pub data_change_count: u64,
    pub error_count: u64,
}

/// Device statistics snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatsSnapshot {
    pub device_id: i32,
    pub name: String,
    pub device_type: String,
    pub state: DeviceState,
    pub data_point_count: usize,
    pub action_count: usize,
    pub metrics: DeviceMetricsSnapshot,
    pub last_collection: Option<DateTime<Utc>>,
    pub last_data_change: Option<DateTime<Utc>>,
    pub uptime: Duration,
}

/// Channel performance metrics snapshot.
#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize)]
pub struct ChannelMetricsSnapshot {
    pub total_operations: u64,
    pub successful_operations: u64,
    pub failed_operations: u64,
    pub average_response_time: Duration,
    pub last_operation_time: Duration,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub reconnection_count: u32,
}

/// Southward manager metrics snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SouthwardManagerMetricsSnapshot {
    /// Channel metrics.
    pub total_channels: usize,
    pub connected_channels: usize,

    /// Device metrics.
    pub total_devices: usize,
    pub active_devices: usize,
    pub total_data_points: usize,
    pub total_actions: usize,
    pub average_points_per_device: f64,

    pub last_update: Option<DateTime<Utc>>,
}

/// Collector metrics snapshot.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CollectorMetricsSnapshot {
    /// Total number of collections started.
    pub total_collections: u64,
    /// Total number of successful collections.
    pub successful_collections: u64,
    /// Total number of failed collections.
    pub failed_collections: u64,
    /// Total number of timeouts.
    pub timeout_collections: u64,
    /// Average collection time in milliseconds.
    pub average_collection_time_ms: f64,
    /// Number of active collection tasks.
    pub active_tasks: usize,
    /// Batch processing efficiency.
    pub batch_efficiency: f64,
    /// Current semaphore permits.
    pub current_permits: usize,
    /// Available semaphore permits.
    pub available_permits: usize,
}

/// Serializable snapshot of app metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NorthwardAppMetricsSnapshot {
    pub messages_sent: u64,
    pub messages_dropped: u64,
    pub errors: u64,
    pub retries: u64,
    pub last_sent: Option<DateTime<Utc>>,
    pub last_error: Option<DateTime<Utc>>,
    pub avg_latency_ms: f64,
}

/// Serializable snapshot of northward manager metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NorthwardManagerMetricsSnapshot {
    pub total_apps: u64,
    pub active_apps: u64,
    pub total_events_received: u64,
    pub total_data_routed: u64,
    pub routing_errors: u64,
    pub last_update: Option<DateTime<Utc>>,
}

// ============================================================================
// Stats Structures (Complete Status + Metrics)
// ============================================================================

/// Complete channel statistics snapshot with state and metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStatsSnapshot {
    /// Channel ID.
    pub channel_id: i32,
    /// Channel name.
    pub name: String,
    /// Driver name.
    pub driver_name: String,
    /// Connection state.
    pub state: SouthwardConnectionState,
    /// Health status.
    pub health: Option<HealthStatus>,
    /// Number of devices on this channel.
    pub device_count: usize,
    /// Performance metrics snapshot.
    pub metrics: ChannelMetricsSnapshot,
    /// Timestamps.
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

/// Complete northward app statistics snapshot with state and metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NorthwardAppStatsSnapshot {
    /// App ID.
    pub app_id: i32,
    /// Plugin ID.
    pub plugin_id: i32,
    /// App name.
    pub name: String,
    /// App state (running, stopped, etc.).
    pub state: AppActorState,
    /// Connection status.
    pub is_connected: bool,
    /// Performance metrics snapshot.
    pub metrics: NorthwardAppMetricsSnapshot,
}

/// App actor state snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum AppActorState {
    /// Not yet started.
    Uninitialized = 0,
    /// Starting up.
    Starting = 1,
    /// Running normally.
    Running = 2,
    /// Stopping.
    Stopping = 3,
    /// Stopped.
    Stopped = 4,
    /// Error state.
    Error = 5,
}

impl From<u8> for AppActorState {
    fn from(value: u8) -> Self {
        match value {
            0 => AppActorState::Uninitialized,
            1 => AppActorState::Starting,
            2 => AppActorState::Running,
            3 => AppActorState::Stopping,
            4 => AppActorState::Stopped,
            5 => AppActorState::Error,
            _ => AppActorState::Error,
        }
    }
}
