use super::{
    super::{NorthwardPublisher, RetryPolicy, Transform},
    types::{AccessMode, CollectionType, DataPointType, DataType, ReportType, Status},
    RuntimeChannel, RuntimeDevice, RuntimePoint,
};
use sea_orm::FromJsonQueryResult;
use serde::{Deserialize, Serialize};
use std::{any::Any, collections::HashMap, sync::Arc, time::Duration};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelModel {
    pub id: i32,
    /// Driver ID
    pub driver_id: i32,
    /// Name
    pub name: String,
    /// Collection type
    pub collection_type: CollectionType,
    /// Period
    pub period: Option<u32>,
    /// Report type
    pub report_type: ReportType,
    /// Status
    pub status: Status,
    /// Connection policy
    pub connection_policy: ConnectionPolicy,
    /// Driver configuration
    pub driver_config: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceModel {
    pub id: i32,
    /// Channel ID
    pub channel_id: i32,
    /// Device Name
    pub device_name: String,
    /// Device Type
    pub device_type: String,
    /// Enabled
    pub status: Status,
    /// Driver configuration
    pub driver_config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointModel {
    pub id: i32,
    /// Device ID
    pub device_id: i32,
    /// Name
    pub name: String,
    /// Key
    pub key: String,
    /// Type
    pub r#type: DataPointType,
    /// Data Type
    pub data_type: DataType,
    /// Access Mode
    pub access_mode: AccessMode,
    /// Unit
    pub unit: Option<String>,
    /// Min Value
    pub min_value: Option<f64>,
    /// Max Value
    pub max_value: Option<f64>,
    /// Logical-layer transformation rules for this point.
    ///
    /// This is always present. The identity transform means:
    /// - `datatype = None` (logical type follows wire `data_type`)
    /// - `scale = None` (treated as 1.0)
    /// - `offset = None` (treated as 0.0)
    /// - `negate = false`
    #[serde(default)]
    pub transform: Transform,
    /// Driver configuration
    pub driver_config: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionModel {
    pub id: i32,
    /// Device ID
    pub device_id: i32,
    /// Name
    pub name: String,
    /// Command
    pub command: String,
    /// Inputs
    pub inputs: Vec<Parameter>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    /// Name
    pub name: String,
    /// Key
    pub key: String,
    /// Data type
    pub data_type: DataType,
    /// Required
    pub required: bool,
    /// Default value
    pub default_value: Option<serde_json::Value>,
    /// Max value
    pub max_value: Option<f64>,
    /// Min value
    pub min_value: Option<f64>,
    /// Logical-layer transformation rules for this parameter.
    ///
    /// This is always present; see `PointModel::transform` for identity semantics.
    #[serde(default)]
    pub transform: Transform,
    /// Driver configuration
    pub driver_config: serde_json::Value,
}

/// Runtime init context for a southbound driver.
///
/// Consolidated view of channel topology plus host-injected capabilities
/// for driver initialization.
///
/// # Three-layer design
///
/// | Layer | Field | Guarantee |
/// |-------|-------|-----------|
/// | **Domain data** | `devices`, `points_by_device`, `runtime_channel`, `channel_id` | Compile-time, always present |
/// | **Core services** | `publisher` | Compile-time, always present |
/// | **Extensions** | `transport_meter`, `observer_factory`, AI engine, … | Runtime, accessor methods with fallbacks |
///
/// Optional infrastructure services (transport metering, supervision observers)
/// live in `extensions`. Convenience accessor methods provide type-safe access
/// with sensible defaults (Noop implementations).
#[derive(Clone)]
pub struct SouthwardInitContext {
    // ── Domain data (always required) ──────────────────────────────
    /// All devices under this channel.
    pub devices: Vec<Arc<dyn RuntimeDevice>>,
    /// Points grouped by device id.
    pub points_by_device: HashMap<i32, Vec<Arc<dyn RuntimePoint>>>,
    /// Runtime channel configuration and policies.
    pub runtime_channel: Arc<dyn RuntimeChannel>,
    /// Channel id (fast access; avoid repeated downcasts).
    pub channel_id: i32,

    // ── Core services (always required) ────────────────────────────
    /// Northbound publisher injected by the host.
    pub publisher: Arc<dyn NorthwardPublisher>,

    // ── Extensions (optional, host-injected) ───────────────────────
    /// Type-erased extension storage for host-injected capabilities.
    ///
    /// Contains optional infrastructure services (transport meter, observer
    /// factory) and domain-specific extensions (AI engine, etc.).
    /// Use [`Extensions::get_or_default`] or [`Extensions::get_cloned`] for access.
    pub extensions: Extensions,
}

// ── Extensions: type-erased capability container ───────────────────

/// A cloneable, type-erased map for injecting host capabilities into drivers.
///
/// Entries are keyed by [`std::any::TypeId`] so each concrete type can appear
/// at most once. Values must be `Send + Sync + 'static` and cloneable (via
/// the internal `Arc` wrapping).
///
/// # Usage
///
/// ```ignore
/// // Host side: insert an AI engine handle
/// let mut ext = Extensions::new();
/// ext.insert(ai_engine_arc.clone());
///
/// // Driver side: retrieve by type
/// if let Some(ai) = ctx.extensions.get_cloned::<Arc<dyn AiEngineApi>>() {
///     ai.analyze_frame(request).await?;
/// }
/// ```
#[derive(Clone, Default)]
pub struct Extensions {
    map: Arc<HashMap<std::any::TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl Extensions {
    /// Create an empty extensions container.
    #[inline]
    pub fn new() -> Self {
        Self {
            map: Arc::new(HashMap::new()),
        }
    }

    /// Insert a value. If a value of the same type already exists, it is replaced.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) {
        let map = Arc::make_mut(&mut self.map);
        map.insert(std::any::TypeId::of::<T>(), Arc::new(value));
    }

    /// Retrieve a reference to a value by its type.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.map
            .get(&std::any::TypeId::of::<T>())
            .and_then(|v| v.downcast_ref::<T>())
    }

    /// Get an extension by type, or produce a default value if not present.
    ///
    /// Use this for extensions that have a sensible no-op fallback (e.g. observer
    /// factory, transport meter). The default is computed lazily via `default`.
    #[inline]
    pub fn get_or_default<T, F>(&self, default: F) -> T
    where
        T: Clone + Send + Sync + 'static,
        F: FnOnce() -> T,
    {
        self.get_cloned::<T>().unwrap_or_else(default)
    }

    /// Get an extension by type as an owned clone, or `None` if not present.
    ///
    /// Use this for optional extensions (e.g. AI engine) where absence is valid.
    #[inline]
    pub fn get_cloned<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.get::<T>().cloned()
    }

    /// Check if a value of the given type exists.
    #[inline]
    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.map.contains_key(&std::any::TypeId::of::<T>())
    }
}

/// Driver metrics
#[derive(Debug, Clone, Default)]
pub struct DriverMetrics {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub average_response_time: Duration,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPolicy {
    #[serde(default = "ConnectionPolicy::default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "ConnectionPolicy::default_read_timeout_ms")]
    pub read_timeout_ms: u64,
    #[serde(default = "ConnectionPolicy::default_write_timeout_ms")]
    pub write_timeout_ms: u64,
    #[serde(default)]
    pub backoff: RetryPolicy,
}

impl ConnectionPolicy {
    fn default_connect_timeout_ms() -> u64 {
        10000
    }
    fn default_read_timeout_ms() -> u64 {
        10000
    }
    fn default_write_timeout_ms() -> u64 {
        10000
    }
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            connect_timeout_ms: ConnectionPolicy::default_connect_timeout_ms(),
            read_timeout_ms: ConnectionPolicy::default_read_timeout_ms(),
            write_timeout_ms: ConnectionPolicy::default_write_timeout_ms(),
            backoff: RetryPolicy::default(),
        }
    }
}

impl sea_orm::IntoActiveValue<ConnectionPolicy> for ConnectionPolicy {
    fn into_active_value(self) -> sea_orm::ActiveValue<ConnectionPolicy> {
        sea_orm::ActiveValue::Set(self)
    }
}
