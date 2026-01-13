use ng_gateway_sdk::PluginConfig;
use serde::{Deserialize, Serialize};

// Re-export SDK types used in configuration
pub use ng_gateway_sdk::northward::downlink::{
    AckPolicy, DownlinkPayloadConfig, EventDownlink, FailurePolicy,
};
pub use ng_gateway_sdk::northward::payload::UplinkPayloadConfig;

/// Pulsar plugin configuration (Phase 1: uplink only)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsarPluginConfig {
    pub connection: PulsarConnectionConfig,
    /// Uplink mappings (Gateway -> Pulsar), by `NorthwardData` kind.
    #[serde(default)]
    pub uplink: UplinkConfig,
    /// Reserved for Phase 2 (Downlink: Pulsar -> Gateway).
    #[serde(default)]
    pub downlink: DownlinkConfig,
}

impl PluginConfig for PulsarPluginConfig {}

/// Pulsar transport configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsarConnectionConfig {
    /// `pulsar://host:6650` or `pulsar+ssl://host:6651`
    pub service_url: String,
    #[serde(default)]
    pub auth: PulsarAuthConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PulsarAuthConfig {
    #[default]
    None,
    Token {
        token: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsarProducerConfig {
    /// Producer compression algorithm.
    ///
    /// NOTE: Defaults to LZ4 for a good throughput/CPU trade-off in gateway workloads.
    #[serde(default)]
    pub compression: PulsarCompression,
    /// Enable batching for throughput.
    ///
    /// NOTE: Defaults to `false` to avoid surprising latency/ordering behaviors for new users.
    #[serde(default = "PulsarProducerConfig::default_batching_enabled")]
    pub batching_enabled: bool,
    /// Flush batch when N messages accumulated.
    ///
    /// NOTE: This value is only applied when `batching_enabled = true`.
    #[serde(default = "PulsarProducerConfig::default_batching_max_messages")]
    pub batching_max_messages: Option<u32>,
    /// Flush batch when total bytes exceed this.
    ///
    /// NOTE: This value is only applied when `batching_enabled = true`.
    #[serde(default = "PulsarProducerConfig::default_batching_max_bytes")]
    pub batching_max_bytes: Option<u32>,
    /// Flush batch when delay exceeds this.
    ///
    /// NOTE: This value is only applied when `batching_enabled = true`.
    #[serde(default = "PulsarProducerConfig::default_batching_max_publish_delay_ms")]
    pub batching_max_publish_delay_ms: Option<u32>,
}

impl PulsarProducerConfig {
    fn default_batching_enabled() -> bool {
        false
    }

    /// Default batch max messages for gateway uplink (only used when batching is enabled).
    #[inline]
    fn default_batching_max_messages() -> Option<u32> {
        Some(1000)
    }

    /// Default batch max bytes for gateway uplink (only used when batching is enabled).
    #[inline]
    fn default_batching_max_bytes() -> Option<u32> {
        // 128 KiB
        Some(128 * 1024)
    }

    /// Default batch max delay for gateway uplink (only used when batching is enabled).
    #[inline]
    fn default_batching_max_publish_delay_ms() -> Option<u32> {
        Some(10)
    }
}

impl Default for PulsarProducerConfig {
    fn default() -> Self {
        Self {
            compression: PulsarCompression::default(),
            batching_enabled: PulsarProducerConfig::default_batching_enabled(),
            batching_max_messages: PulsarProducerConfig::default_batching_max_messages(),
            batching_max_bytes: PulsarProducerConfig::default_batching_max_bytes(),
            batching_max_publish_delay_ms:
                PulsarProducerConfig::default_batching_max_publish_delay_ms(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PulsarCompression {
    None,
    #[default]
    Lz4,
    Zlib,
    Zstd,
    Snappy,
}

/// Default `true` helper for config toggles.
#[inline]
fn default_enabled_true() -> bool {
    true
}

// ===== mappings (Phase 1) =====

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UplinkConfig {
    /// Master switch for uplink mappings.
    ///
    /// NOTE: Defaults to `true` to keep backward compatibility when older configs
    /// do not contain this field.
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    /// Producer settings for uplink publish.
    ///
    /// This config block always exists. Leave individual fields unset (null) to use plugin defaults.
    #[serde(default)]
    pub producer: PulsarProducerConfig,
    #[serde(default = "EventUplink::default_device_connected")]
    pub device_connected: EventUplink,
    #[serde(default = "EventUplink::default_device_disconnected")]
    pub device_disconnected: EventUplink,
    #[serde(default = "EventUplink::default_telemetry")]
    pub telemetry: EventUplink,
    #[serde(default = "EventUplink::default_attributes")]
    pub attributes: EventUplink,
}

impl Default for UplinkConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            producer: PulsarProducerConfig::default(),
            device_connected: EventUplink::default_device_connected(),
            device_disconnected: EventUplink::default_device_disconnected(),
            telemetry: EventUplink::default_telemetry(),
            attributes: EventUplink::default_attributes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventUplink {
    #[serde(default)]
    pub enabled: bool,
    /// Topic template for uplink publish.
    #[serde(default = "EventUplink::default_uplink_topic")]
    pub topic: String,
    /// Message key template (Pulsar partition_key).
    #[serde(default = "EventUplink::default_uplink_key")]
    pub key: String,
    #[serde(default)]
    pub payload: UplinkPayloadConfig,
}

impl Default for EventUplink {
    fn default() -> Self {
        Self {
            enabled: false,
            topic: EventUplink::default_uplink_topic(),
            key: EventUplink::default_uplink_key(),
            payload: UplinkPayloadConfig::default(),
        }
    }
}

impl EventUplink {
    fn default_uplink_topic() -> String {
        // User-approved default
        "persistent://public/default/ng.uplink.{{event_kind}}.{{device_name}}".to_string()
    }

    fn default_uplink_key() -> String {
        "{{device_id}}".to_string()
    }

    fn default_device_connected() -> Self {
        Self::default()
    }

    fn default_device_disconnected() -> Self {
        Self::default()
    }

    fn default_telemetry() -> Self {
        Self::default()
    }

    fn default_attributes() -> Self {
        Self::default()
    }
}

/// Phase 2: Downlink (Pulsar -> Gateway)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownlinkConfig {
    /// Master switch for downlink subscription and mappings.
    ///
    /// NOTE: Defaults to `true` to keep backward compatibility when older configs
    /// do not contain this field.
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    /// Downlink mappings (Pulsar -> Gateway), by `NorthwardEvent` kind.
    #[serde(default = "default_write_point")]
    pub write_point: EventDownlink,
    #[serde(default = "default_command_received")]
    pub command_received: EventDownlink,
    #[serde(default = "default_rpc_response_received")]
    pub rpc_response_received: EventDownlink,
}

impl Default for DownlinkConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            write_point: default_write_point(),
            command_received: default_command_received(),
            rpc_response_received: default_rpc_response_received(),
        }
    }
}

fn default_write_point() -> EventDownlink {
    EventDownlink {
        topic: "persistent://public/default/ng.downlink".to_string(),
        ..Default::default()
    }
}

fn default_command_received() -> EventDownlink {
    EventDownlink {
        topic: "persistent://public/default/ng.downlink".to_string(),
        ..Default::default()
    }
}

fn default_rpc_response_received() -> EventDownlink {
    EventDownlink {
        topic: "persistent://public/default/ng.downlink".to_string(),
        ..Default::default()
    }
}
