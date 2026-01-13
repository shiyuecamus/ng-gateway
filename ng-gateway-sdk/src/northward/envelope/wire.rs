use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

/// Stable JSON envelope used by northward plugins for interop.
///
/// # Design
/// - **Stable**: the top-level shape is versioned with `schema_version`.
/// - **Routable**: `event.kind` is a stable string discriminator for mixed-topic scenarios.
/// - **Extensible**: `envelope` metadata is optional to allow minimal downlink inputs.
///
/// # JSON shape (schema_version = 1)
/// ```json
/// {
///   "schema_version": 1,
///   "event": { "kind": "telemetry" },
///   "envelope": {
///     "ts_ms": 1734870900000,
///     "app": { "id": 1, "name": "my-app", "plugin_type": "pulsar" },
///     "device": { "id": 1001, "name": "dev-1", "type": null },
///     "channel": { "name": "default" }
///   },
///   "payload": { "data": {} }
/// }
/// ```
///
/// # Important
/// The payload is intentionally stored under `payload.data` **without enum tagging**.
/// The discriminator is always `event.kind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireEnvelope<T> {
    /// Schema version for evolution.
    pub schema_version: u32,
    /// Event discriminator used for routing/matching.
    pub event: EnvelopeEvent,
    /// Optional metadata envelope (uplink typically includes it; downlink may omit it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<EnvelopeMeta>,
    /// Payload wrapper.
    pub payload: WirePayload<T>,
}

impl<T> WireEnvelope<T> {
    /// Create an envelope with a specific schema version and event kind.
    #[inline]
    pub fn new(schema_version: u32, kind: EnvelopeKind, data: T) -> Self {
        Self {
            schema_version,
            event: EnvelopeEvent { kind },
            envelope: None,
            payload: WirePayload { data },
        }
    }

    /// Create a v1 envelope.
    #[inline]
    pub fn v1(kind: EnvelopeKind, data: T) -> Self {
        Self::new(1, kind, data)
    }

    /// Attach envelope metadata.
    #[inline]
    pub fn with_meta(mut self, meta: EnvelopeMeta) -> Self {
        self.envelope = Some(meta);
        self
    }
}

/// Envelope event discriminator.
#[derive(Debug, Clone, Serialize, Deserialize, Copy)]
pub struct EnvelopeEvent {
    /// Stable event kind string (snake_case).
    pub kind: EnvelopeKind,
}

/// Stable envelope kind discriminator.
///
/// # Compatibility mode
/// The current implementation is **strict**: unknown kinds are rejected by serde deserialization.
///
/// If you need rolling-upgrade compatibility (preserve unknown kinds), introduce a separate
/// "compat kind" wrapper (e.g. `EnvelopeKindExt`) or gate an `Other(String)` variant behind a
/// feature flag.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeKind {
    // ===== uplink kinds =====
    DeviceConnected,
    DeviceDisconnected,
    Telemetry,
    Attributes,
    Alarm,
    RpcResponse,
    WritePointResponse,
    // ===== downlink kinds =====
    WritePoint,
    CommandReceived,
    RpcResponseReceived,
}

impl Display for EnvelopeKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl EnvelopeKind {
    /// Return the stable string representation (snake_case).
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        match self {
            EnvelopeKind::DeviceConnected => "device_connected",
            EnvelopeKind::DeviceDisconnected => "device_disconnected",
            EnvelopeKind::Telemetry => "telemetry",
            EnvelopeKind::Attributes => "attributes",
            EnvelopeKind::Alarm => "alarm",
            EnvelopeKind::RpcResponse => "rpc_response",
            EnvelopeKind::WritePointResponse => "write_point_response",
            EnvelopeKind::WritePoint => "write_point",
            EnvelopeKind::CommandReceived => "command_received",
            EnvelopeKind::RpcResponseReceived => "rpc_response_received",
        }
    }
}

/// Envelope payload wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePayload<T> {
    /// Event payload data.
    pub data: T,
}

/// Metadata envelope for observability and templating.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeMeta {
    /// Timestamp (unix ms).
    pub ts_ms: i64,
    /// Application identity.
    pub app: EnvelopeApp,
    /// Device identity.
    pub device: EnvelopeDevice,
}

/// Application metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeApp {
    /// App id in gateway.
    pub id: i32,
    /// App name.
    pub name: String,
    /// Plugin type string (e.g. "pulsar").
    pub plugin_type: String,
}

/// Device metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeDevice {
    /// Device id in gateway.
    pub id: i32,
    /// Device name.
    pub name: String,
    /// Optional device type.
    #[serde(default)]
    pub r#type: Option<String>,
}

/// Channel metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeChannel {
    /// Channel name.
    pub name: String,
}
