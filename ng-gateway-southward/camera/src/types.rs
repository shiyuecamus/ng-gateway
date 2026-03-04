//! Camera driver runtime types.
//!
//! Domain models for the Camera southward driver:
//! - [`CameraChannel`]: channel-level config (protocol, pipeline, sampling)
//! - [`CameraDevice`]: lightweight device (just identity + status)
//! - [`CameraPoint`]: AI output mapping point (detection_count, person_count, …)
//! - [`CameraAction`]: camera-specific actions (PTZ, snapshot, restart pipeline)

use ng_gateway_ai::api::SamplingStrategy;
use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    DriverError, ReportType, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimeParameter,
    RuntimePoint, Status, Transform,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ─── Channel ───────────────────────────────────────────────────────

/// Camera channel — carries all protocol and pipeline configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraChannel {
    pub id: i32,
    pub name: String,
    pub driver_id: i32,
    pub collection_type: CollectionType,
    pub report_type: ReportType,
    pub period: Option<u32>,
    pub status: Status,
    pub connection_policy: ConnectionPolicy,
    /// Camera-specific configuration.
    pub config: CameraChannelConfig,
}

/// Camera channel configuration (deserialized from `driver_config` JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraChannelConfig {
    /// Video source protocol configuration.
    pub protocol: CameraProtocol,
    /// AI pipeline identifier for this camera channel.
    pub pipeline_id: i32,
    /// Frame sampling strategy.
    #[serde(default)]
    pub sampling: SamplingStrategy,
}

impl DriverConfig for CameraChannelConfig {}

/// Supported camera connection protocols.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CameraProtocol {
    /// RTSP live video stream.
    Rtsp {
        /// Full RTSP URL (e.g., `rtsp://admin:pass@192.168.1.100:554/stream1`).
        url: url::Url,
        /// Transport layer: TCP (reliable, default) or UDP (low latency).
        #[serde(default)]
        transport: RtspTransport,
    },
    /// ONVIF device discovery and media streaming.
    Onvif {
        /// ONVIF device service endpoint.
        endpoint: url::Url,
        /// ONVIF media profile token (empty = auto-select first).
        #[serde(default)]
        profile: String,
        /// Authentication username.
        username: Option<String>,
        /// Authentication password.
        password: Option<String>,
        /// RTSP transport layer for the discovered stream (default: TCP).
        #[serde(default)]
        transport: RtspTransport,
    },
    /// HTTP MJPEG stream.
    Mjpeg {
        /// MJPEG stream URL.
        url: url::Url,
    },
}

/// RTSP transport layer selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RtspTransport {
    #[default]
    Tcp,
    Udp,
}

impl RuntimeChannel for CameraChannel {
    fn id(&self) -> i32 {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn driver_id(&self) -> i32 {
        self.driver_id
    }

    fn collection_type(&self) -> CollectionType {
        self.collection_type
    }

    fn report_type(&self) -> ReportType {
        self.report_type
    }

    fn period(&self) -> Option<u32> {
        self.period
    }

    fn status(&self) -> Status {
        self.status
    }

    fn connection_policy(&self) -> &ConnectionPolicy {
        &self.connection_policy
    }

    fn config(&self) -> &dyn DriverConfig {
        &self.config
    }
}

// ─── Device ────────────────────────────────────────────────────────

/// Camera device — lightweight identity wrapper.
///
/// Camera devices are "virtual" in the sense that the physical device is
/// the camera stream itself (one stream per channel). Devices partition
/// AI output points for northward reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraDevice {
    pub id: i32,
    pub channel_id: i32,
    pub device_name: String,
    pub device_type: String,
    pub status: Status,
}

impl RuntimeDevice for CameraDevice {
    fn id(&self) -> i32 {
        self.id
    }

    fn device_name(&self) -> &str {
        &self.device_name
    }

    fn device_type(&self) -> &str {
        &self.device_type
    }

    fn channel_id(&self) -> i32 {
        self.channel_id
    }

    fn status(&self) -> Status {
        self.status
    }
}

// ─── Point ─────────────────────────────────────────────────────────

/// Camera point — maps an AI analysis output to a northward data point.
///
/// The `output_key` determines which analysis result field this point represents:
/// - `detection_count` → total number of detected objects
/// - `person_count` → count of detections with class "person"
/// - `vehicle_count` → count of detections with class "car"|"truck"|"bus"
/// - `inference_latency_ms` → pipeline inference latency in milliseconds
/// - `detection_json` → full detection array serialized as JSON
/// - `alarm_active` → boolean indicating active alarms
/// - `top_class` → highest-confidence classification label
/// - `top_confidence` → highest classification confidence score
/// - `custom` → user-defined expression (Phase 3+)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraPoint {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub key: String,
    pub r#type: DataPointType,
    pub data_type: DataType,
    pub access_mode: AccessMode,
    pub unit: Option<String>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    #[serde(default)]
    pub transform: Transform,
    /// AI output key that this point is mapped to.
    pub output_key: CameraOutputKey,
    /// Optional custom expression (only for `CameraOutputKey::Custom`).
    pub custom_expression: Option<String>,
}

/// Predefined AI output key mappings for camera points.
///
/// Each variant has a stable wire-format string ([`as_str`]) that is used in
/// both the UI schema (`EnumItem::key`) and database `driver_config` JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraOutputKey {
    /// Total number of detected objects.
    DetectionCount,
    /// Number of "person" class detections.
    PersonCount,
    /// Number of vehicle class detections ("car", "truck", "bus").
    VehicleCount,
    /// Pipeline inference latency in milliseconds.
    InferenceLatencyMs,
    /// Full detection array as JSON string.
    DetectionJson,
    /// Whether any alarm is currently active.
    AlarmActive,
    /// Top classification label (highest confidence).
    TopClass,
    /// Top classification confidence score.
    TopConfidence,
    /// User-defined custom expression (Phase 3+).
    Custom,
}

impl CameraOutputKey {
    /// Wire-format string (matches UiSchema `outputKey` values and DB JSON).
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DetectionCount => "detection_count",
            Self::PersonCount => "person_count",
            Self::VehicleCount => "vehicle_count",
            Self::InferenceLatencyMs => "inference_latency_ms",
            Self::DetectionJson => "detection_json",
            Self::AlarmActive => "alarm_active",
            Self::TopClass => "top_class",
            Self::TopConfidence => "top_confidence",
            Self::Custom => "custom",
        }
    }

    /// All variants in definition order (for UI schema generation).
    pub const ALL: &'static [Self] = &[
        Self::DetectionCount,
        Self::PersonCount,
        Self::VehicleCount,
        Self::InferenceLatencyMs,
        Self::DetectionJson,
        Self::AlarmActive,
        Self::TopClass,
        Self::TopConfidence,
        Self::Custom,
    ];
}

impl TryFrom<&str> for CameraOutputKey {
    type Error = DriverError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .iter()
            .find(|v| v.as_str() == value)
            .copied()
            .ok_or_else(|| {
                let valid = Self::ALL
                    .iter()
                    .map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                DriverError::ConfigurationError(format!(
                    "Unknown camera output key: '{value}'. Expected one of: {valid}"
                ))
            })
    }
}

impl std::fmt::Display for CameraOutputKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl RuntimePoint for CameraPoint {
    fn id(&self) -> i32 {
        self.id
    }

    fn device_id(&self) -> i32 {
        self.device_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn key(&self) -> &str {
        &self.key
    }

    fn r#type(&self) -> DataPointType {
        self.r#type
    }

    fn data_type(&self) -> DataType {
        self.data_type
    }

    fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }

    fn min_value(&self) -> Option<f64> {
        self.min_value
    }

    fn max_value(&self) -> Option<f64> {
        self.max_value
    }

    fn transform(&self) -> &Transform {
        &self.transform
    }
}

// ─── Action ────────────────────────────────────────────────────────

/// Camera action — PTZ control, snapshot, pipeline restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraAction {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    /// Typed command (parsed at converter boundary, never stale).
    pub command: CameraCommand,
    pub input_parameters: Vec<CameraParameter>,
}

/// Typed camera command — replaces string matching with exhaustive enum.
///
/// Parsed once at the converter boundary ([`SouthwardModelConverter`]),
/// consumed by [`CameraHandle::execute_action`] via `match`. Adding a
/// new variant is a compile error in every consumer until handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraCommand {
    /// Start continuous PTZ movement (pan/tilt/zoom velocities).
    PtzMove,
    /// Stop all PTZ movement.
    PtzStop,
    /// Move to a saved PTZ preset position.
    PtzPreset,
    /// Capture the latest annotated AI snapshot.
    Snapshot,
    /// Reset the AI analysis pipeline.
    RestartPipeline,
}

impl CameraCommand {
    /// Wire-format string (matches UiSchema `actionType` values and DB JSON).
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PtzMove => "ptz_move",
            Self::PtzStop => "ptz_stop",
            Self::PtzPreset => "ptz_preset",
            Self::Snapshot => "snapshot",
            Self::RestartPipeline => "restart_pipeline",
        }
    }

    /// All variants in definition order (for UI schema generation).
    pub const ALL: &'static [Self] = &[
        Self::PtzMove,
        Self::PtzStop,
        Self::PtzPreset,
        Self::Snapshot,
        Self::RestartPipeline,
    ];
}

impl TryFrom<&str> for CameraCommand {
    type Error = DriverError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .iter()
            .find(|v| v.as_str() == value)
            .copied()
            .ok_or_else(|| {
                let valid = Self::ALL
                    .iter()
                    .map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                DriverError::ConfigurationError(format!(
                    "Unknown camera command: '{value}'. Expected one of: {valid}"
                ))
            })
    }
}

impl std::fmt::Display for CameraCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Camera action parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraParameter {
    pub name: String,
    pub key: String,
    pub data_type: DataType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,
    #[serde(default)]
    pub transform: Transform,
}

impl RuntimeParameter for CameraParameter {
    fn name(&self) -> &str {
        &self.name
    }

    fn key(&self) -> &str {
        &self.key
    }

    fn data_type(&self) -> DataType {
        self.data_type
    }

    fn required(&self) -> bool {
        self.required
    }

    fn default_value(&self) -> Option<serde_json::Value> {
        self.default_value.clone()
    }

    fn max_value(&self) -> Option<f64> {
        self.max_value
    }

    fn min_value(&self) -> Option<f64> {
        self.min_value
    }

    fn transform(&self) -> &Transform {
        &self.transform
    }
}

impl RuntimeAction for CameraAction {
    fn id(&self) -> i32 {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn device_id(&self) -> i32 {
        self.device_id
    }

    fn command(&self) -> &str {
        self.command.as_str()
    }

    fn input_parameters(&self) -> Vec<Arc<dyn RuntimeParameter>> {
        self.input_parameters
            .iter()
            .map(|p| Arc::new(p.clone()) as Arc<dyn RuntimeParameter>)
            .collect()
    }
}
