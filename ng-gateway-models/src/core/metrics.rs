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
#[serde(rename_all = "camelCase")]
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
    /// Gateway process RSS memory in bytes (best-effort).
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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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

/// Channel performance metrics snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelMetricsSnapshot {
    /// Total southward I/O operations (collect + control-plane), best-effort.
    pub total_operations: u64,
    pub successful_operations: u64,
    pub failed_operations: u64,
    pub average_response_time: Duration,
    pub last_operation_time: Duration,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub reconnection_count: u32,

    // --- collection (point-based) ---
    /// Total points read successfully by collection (cumulative).
    pub point_read_success_total: u64,
    /// Total points read failed by collection (cumulative).
    pub point_read_fail_total: u64,
    /// Total points read timed out by collection (cumulative).
    pub point_read_timeout_total: u64,

    // --- connection reliability ---
    /// Total times the channel entered `Failed(_)` (cumulative).
    pub connect_failed_count: u64,
    /// Total disconnect transitions from `Connected` (cumulative).
    pub disconnect_count: u64,
    /// Last connection state change time (best-effort).
    pub last_state_change_at: Option<DateTime<Utc>>,

    // --- report/push (publisher.try_publish) ---
    /// Total successful publish attempts from driver (cumulative).
    pub report_publish_success_total: u64,
    /// Total dropped publish attempts due to backpressure (QueueFull) (cumulative).
    pub report_publish_dropped_total: u64,
    /// Total failed publish attempts (Closed/other) (cumulative).
    pub report_publish_fail_total: u64,
    /// Last successfully published report time (best-effort).
    pub last_report_at: Option<DateTime<Utc>>,
}

/// Control-plane metrics snapshot for one southward channel.
#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlMetricsSnapshot {
    // write-point
    pub write_success_total: u64,
    pub write_fail_total: u64,
    pub write_timeout_total: u64,
    /// Average queue wait time (ms), derived from histogram sum/count (best-effort).
    pub write_queue_wait_avg_ms: f64,
    /// Average driver execution time (ms), derived from histogram sum/count (best-effort).
    pub write_execute_avg_ms: f64,

    // execute-action
    pub execute_success_total: u64,
    pub execute_fail_total: u64,
    pub execute_timeout_total: u64,
    /// Average driver execution time (ms), derived from histogram sum/count (best-effort).
    pub execute_avg_ms: f64,
}

/// Southward per-device metrics snapshot (in-memory / WS / REST use-cases).
///
/// # Notes
/// - This is a **snapshot DTO**; it must remain fully serializable.
/// - Per-device data MUST NOT be exposed as Prometheus labels (cardinality).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeviceMetricsSnapshot {
    pub collect_success_total: u64,
    pub collect_fail_total: u64,
    pub collect_timeout_total: u64,

    /// EWMA of collection latency (best-effort), milliseconds.
    pub avg_collect_latency_ms: u64,
    /// Last collection latency (best-effort), milliseconds.
    pub last_collect_latency_ms: u64,

    // --- report/push (publisher.try_publish) ---
    pub report_success_total: u64,
    pub report_dropped_total: u64,
    pub report_fail_total: u64,
    /// Unix timestamp in milliseconds (best-effort).
    pub last_report_ms: u64,

    /// Unix timestamp in milliseconds (best-effort).
    pub last_activity_ms: u64,
}

/// Southward channel per-device observability row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatsSnapshot {
    pub device_id: i32,
    pub channel_id: i32,
    pub device_name: String,
    pub device_type: String,
    /// Device enablement status (0=enabled, 1=disabled). Matches `ng_gateway_sdk::Status` repr.
    pub status: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_state: Option<DeviceState>,

    #[serde(flatten)]
    pub metrics: DeviceMetricsSnapshot,
}

/// Southward manager metrics snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
    /// Total retries performed by the collector (cumulative).
    pub retries_total: u64,
    /// Retries triggered by timeouts (cumulative).
    pub retries_timeout_total: u64,
    /// Retries triggered by driver errors (cumulative).
    pub retries_error_total: u64,
}

/// Serializable snapshot of app metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
    /// Optional control-plane metrics snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_metrics: Option<ControlMetricsSnapshot>,
    /// Timestamps.
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

/// Complete northward app statistics snapshot with state and metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
