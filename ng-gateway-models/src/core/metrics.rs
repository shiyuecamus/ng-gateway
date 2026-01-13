use crate::enums::core::GatewayState;
use chrono::{DateTime, Duration, Utc};
use ng_gateway_sdk::{DeviceState, HealthStatus, SouthwardConnectionState};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    RwLock,
};

/// Comprehensive gateway metrics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayMetrics {
    /// System uptime
    pub uptime: Duration,

    /// Channel metrics
    pub total_channels: usize,
    pub connected_channels: usize,

    /// Device metrics
    pub total_devices: usize,
    pub active_devices: usize,
    pub total_data_points: usize,

    /// Collection metrics
    pub total_collections: u64, // Total number of collections started
    pub successful_collections: u64, // Total number of successful collections
    pub failed_collections: u64,     // Total number of failed collections
    pub timeout_collections: u64,    // Total number of timeouts
    pub average_collection_time_ms: f64, // Average collection time in milliseconds
    pub active_tasks: usize,         // Number of active collection tasks

    /// Performance metrics
    pub memory_usage: u64,
    pub cpu_usage: f64,
    pub network_bytes_sent: u64,
    pub network_bytes_received: u64,

    /// Error metrics
    pub total_errors: u64,
    pub error_rate: f64, // errors per minute

    /// Last update timestamp
    pub last_update: Option<DateTime<Utc>>,
}

/// Comprehensive gateway status information
#[derive(Debug, Clone)]
pub struct GatewayStatus {
    /// Gateway running state
    pub state: GatewayState,
    /// Gateway-level aggregated metrics
    pub metrics: GatewayMetrics,
    /// Southward (data collection) subsystem metrics
    pub southward_metrics: SouthwardManagerMetrics,
    /// Northward (data forwarding) subsystem metrics
    pub northward_metrics: NorthwardManagerMetricsSnapshot,
    /// Collection engine metrics
    pub collector_metrics: CollectorMetrics,
    /// Gateway version
    pub version: String,
    /// System information
    pub system_info: SystemInfo,
}

impl GatewayStatus {
    /// Get a serializable snapshot of the gateway status
    pub fn get_snapshot(&self) -> GatewayStatusSnapshot {
        GatewayStatusSnapshot {
            state: self.state.clone(),
            metrics: self.metrics.clone(),
            southward_metrics: self.southward_metrics.clone(),
            northward_metrics: self.northward_metrics.clone(),
            collector_metrics: self.collector_metrics.clone(),
            version: self.version.clone(),
            system_info: self.system_info.clone(),
        }
    }
}

/// Gateway status snapshot (fully serializable)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayStatusSnapshot {
    /// Gateway running state
    pub state: GatewayState,
    /// Gateway-level aggregated metrics
    pub metrics: GatewayMetrics,
    /// Southward (data collection) subsystem metrics
    pub southward_metrics: SouthwardManagerMetrics,
    /// Northward (data forwarding) subsystem metrics
    pub northward_metrics: NorthwardManagerMetricsSnapshot,
    /// Collection engine metrics
    pub collector_metrics: CollectorMetrics,
    /// Gateway version
    pub version: String,
    /// System information
    pub system_info: SystemInfo,
}

/// System information including real-time metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    /// Operating system type (e.g., "Linux", "Windows", "macOS")
    pub os_type: String,
    /// Operating system architecture (e.g., "x86_64", "aarch64")
    pub os_arch: String,
    /// Hostname
    pub hostname: Option<String>,
    /// Number of CPU cores
    pub cpu_cores: usize,
    /// Total memory in bytes
    pub total_memory: u64,
    /// Used memory in bytes
    pub used_memory: u64,
    /// Memory usage percentage (0-100)
    pub memory_usage_percent: f64,
    /// CPU usage percentage (0-100)
    pub cpu_usage_percent: f64,
    /// Total disk space in bytes
    pub total_disk: u64,
    /// Used disk space in bytes
    pub used_disk: u64,
    /// Disk usage percentage (0-100)
    pub disk_usage_percent: f64,
}

/// Device performance metrics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceMetrics {
    pub total_collections: u64,
    pub successful_collections: u64,
    pub failed_collections: u64,
    pub data_points_collected: u64,
    pub average_collection_time: Duration,
    pub last_collection_time: Duration,
    pub data_change_count: u64,
    pub error_count: u64,
}

/// Device statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStats {
    pub device_id: i32,
    pub name: String,
    pub device_type: String,
    pub state: DeviceState,
    pub data_point_count: usize,
    pub action_count: usize,
    pub metrics: DeviceMetrics,
    pub last_collection: Option<DateTime<Utc>>,
    pub last_data_change: Option<DateTime<Utc>>,
    pub uptime: Duration,
}

/// Channel performance metrics
#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize)]
pub struct ChannelMetrics {
    pub total_operations: u64,
    pub successful_operations: u64,
    pub failed_operations: u64,
    pub average_response_time: Duration,
    pub last_operation_time: Duration,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub reconnection_count: u32,
}

/// Data manager metrics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SouthwardManagerMetrics {
    // Channel metrics
    pub total_channels: usize,
    pub connected_channels: usize,

    // Device metrics
    pub total_devices: usize,
    pub active_devices: usize,
    pub total_data_points: usize,
    pub total_actions: usize,
    pub average_points_per_device: f64,

    pub last_update: Option<DateTime<Utc>>,
}

/// Enhanced collection engine metrics
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CollectorMetrics {
    /// Total number of collections started
    pub total_collections: u64,
    /// Total number of successful collections
    pub successful_collections: u64,
    /// Total number of failed collections
    pub failed_collections: u64,
    /// Total number of timeouts
    pub timeout_collections: u64,
    /// Average collection time in milliseconds
    pub average_collection_time_ms: f64,
    /// Number of active collection tasks
    pub active_tasks: usize,
    /// Batch processing efficiency
    pub batch_efficiency: f64,
    /// Current semaphore permits
    pub current_permits: usize,
    /// Available semaphore permits
    pub available_permits: usize,
}

// ============================================================================
// Northward System Metrics
// ============================================================================

/// App-level metrics with atomic counters for lock-free updates
///
/// Used internally by AppActor to track per-app performance metrics
#[derive(Debug)]
pub struct NorthwardAppMetrics {
    /// Total messages successfully sent to the platform
    pub messages_sent: AtomicU64,

    /// Total messages dropped (due to queue full or other reasons)
    pub messages_dropped: AtomicU64,

    /// Total errors encountered
    pub errors: AtomicU64,

    /// Total retries attempted
    pub retries: AtomicU64,

    /// Last successful send timestamp (requires lock for DateTime)
    pub last_sent: RwLock<Option<DateTime<Utc>>>,

    /// Last error timestamp
    pub last_error: RwLock<Option<DateTime<Utc>>>,

    /// Average latency in nanoseconds (stored as u64 for atomic access)
    pub avg_latency_ns: AtomicU64,
}

impl Default for NorthwardAppMetrics {
    fn default() -> Self {
        Self {
            messages_sent: AtomicU64::new(0),
            messages_dropped: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            retries: AtomicU64::new(0),
            last_sent: RwLock::new(None),
            last_error: RwLock::new(None),
            avg_latency_ns: AtomicU64::new(0),
        }
    }
}

impl NorthwardAppMetrics {
    /// Increment messages sent counter (lock-free)
    #[inline]
    pub fn increment_sent(&self) {
        self.messages_sent.fetch_add(1, Ordering::Relaxed);
        *self.last_sent.write().unwrap() = Some(Utc::now());
    }

    /// Increment messages dropped counter (lock-free)
    #[inline]
    pub fn increment_dropped(&self) {
        self.messages_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment errors counter (lock-free)
    #[inline]
    pub fn increment_errors(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        *self.last_error.write().unwrap() = Some(Utc::now());
    }

    /// Increment retries counter (lock-free)
    #[inline]
    pub fn increment_retries(&self) {
        self.retries.fetch_add(1, Ordering::Relaxed);
    }

    /// Update average latency using exponential moving average
    ///
    /// Uses a simple EMA: new_avg = 0.9 * old_avg + 0.1 * sample
    #[inline]
    pub fn update_latency(&self, sample_ns: u64) {
        let old = self.avg_latency_ns.load(Ordering::Relaxed);
        let new = if old == 0 {
            sample_ns
        } else {
            // EMA with alpha = 0.1
            (old * 9 + sample_ns) / 10
        };
        self.avg_latency_ns.store(new, Ordering::Relaxed);
    }

    /// Get a consistent snapshot of all metrics
    pub fn snapshot(&self) -> NorthwardAppMetricsSnapshot {
        NorthwardAppMetricsSnapshot {
            messages_sent: self.messages_sent.load(Ordering::Relaxed),
            messages_dropped: self.messages_dropped.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
            last_sent: *self.last_sent.read().unwrap(),
            last_error: *self.last_error.read().unwrap(),
            avg_latency_ms: self.avg_latency_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0,
        }
    }

    /// Reset all metrics (useful for testing)
    #[cfg(test)]
    pub fn reset(&self) {
        self.messages_sent.store(0, Ordering::Relaxed);
        self.messages_dropped.store(0, Ordering::Relaxed);
        self.errors.store(0, Ordering::Relaxed);
        self.retries.store(0, Ordering::Relaxed);
        *self.last_sent.write().unwrap() = None;
        *self.last_error.write().unwrap() = None;
        self.avg_latency_ns.store(0, Ordering::Relaxed);
    }
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

/// Manager-level metrics with atomic counters
#[derive(Debug)]
pub struct NorthwardManagerMetrics {
    /// Total number of apps (enabled + disabled)
    pub total_apps: AtomicU64,

    /// Number of currently active (running) apps
    pub active_apps: AtomicU64,

    /// Total events received from all apps
    pub total_events_received: AtomicU64,

    /// Total data items routed to apps
    pub total_data_routed: AtomicU64,

    /// Total routing errors
    pub routing_errors: AtomicU64,

    /// Last update timestamp
    pub last_update: RwLock<Option<DateTime<Utc>>>,
}

impl Default for NorthwardManagerMetrics {
    fn default() -> Self {
        Self {
            total_apps: AtomicU64::new(0),
            active_apps: AtomicU64::new(0),
            total_events_received: AtomicU64::new(0),
            total_data_routed: AtomicU64::new(0),
            routing_errors: AtomicU64::new(0),
            last_update: RwLock::new(None),
        }
    }
}

impl NorthwardManagerMetrics {
    /// Set total apps count
    #[inline]
    pub fn set_total_apps(&self, count: u64) {
        self.total_apps.store(count, Ordering::Relaxed);
        self.touch();
    }

    /// Set active apps count
    #[inline]
    pub fn set_active_apps(&self, count: u64) {
        self.active_apps.store(count, Ordering::Relaxed);
        self.touch();
    }

    /// Increment events received counter
    #[inline]
    pub fn increment_events_received(&self) {
        self.total_events_received.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    /// Increment data routed counter
    #[inline]
    pub fn increment_data_routed(&self) {
        self.total_data_routed.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    /// Increment routing errors counter
    #[inline]
    pub fn increment_routing_errors(&self) {
        self.routing_errors.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    /// Update last_update timestamp
    #[inline]
    fn touch(&self) {
        *self.last_update.write().unwrap() = Some(Utc::now());
    }

    /// Get a consistent snapshot of all metrics
    pub fn snapshot(&self) -> NorthwardManagerMetricsSnapshot {
        NorthwardManagerMetricsSnapshot {
            total_apps: self.total_apps.load(Ordering::Relaxed),
            active_apps: self.active_apps.load(Ordering::Relaxed),
            total_events_received: self.total_events_received.load(Ordering::Relaxed),
            total_data_routed: self.total_data_routed.load(Ordering::Relaxed),
            routing_errors: self.routing_errors.load(Ordering::Relaxed),
            last_update: *self.last_update.read().unwrap(),
        }
    }
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

/// Complete channel statistics with state and metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStats {
    /// Channel ID
    pub channel_id: i32,
    /// Channel name
    pub name: String,
    /// Driver name
    pub driver_name: String,
    /// Connection state
    pub state: SouthwardConnectionState,
    /// Health status
    pub health: Option<HealthStatus>,
    /// Number of devices on this channel
    pub device_count: usize,
    /// Performance metrics
    pub metrics: ChannelMetrics,
    /// Timestamps
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

/// Complete northward app statistics with state and metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NorthwardAppStats {
    /// App ID
    pub app_id: i32,
    /// Plugin ID
    pub plugin_id: i32,
    /// App name
    pub name: String,
    /// App state (running, stopped, etc.)
    pub state: AppState,
    /// Connection status
    pub is_connected: bool,
    /// Performance metrics
    pub metrics: NorthwardAppMetricsSnapshot,
}

/// App actor state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum AppState {
    /// Not yet started
    Uninitialized = 0,
    /// Starting up
    Starting = 1,
    /// Running normally
    Running = 2,
    /// Stopping
    Stopping = 3,
    /// Stopped
    Stopped = 4,
    /// Error state
    Error = 5,
}

impl From<u8> for AppState {
    fn from(value: u8) -> Self {
        match value {
            0 => AppState::Uninitialized,
            1 => AppState::Starting,
            2 => AppState::Running,
            3 => AppState::Stopping,
            4 => AppState::Stopped,
            5 => AppState::Error,
            _ => AppState::Error,
        }
    }
}
