//! Configuration types for the Kafka northward plugin.
//!
//! Notes:
//! - All config structs are `Serialize`/`Deserialize` and compatible with the gateway UI schema.
//! - This module intentionally mirrors the shape of `ng-plugin-pulsar` for consistent UX.

use ng_gateway_sdk::PluginConfig;
use serde::{Deserialize, Serialize};

// Re-export SDK types used in configuration
pub use ng_gateway_sdk::northward::downlink::{
    AckPolicy, DownlinkPayloadConfig, EventDownlink, FailurePolicy,
};
pub use ng_gateway_sdk::northward::payload::UplinkPayloadConfig;

/// Kafka plugin configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KafkaPluginConfig {
    /// Transport-level connection and security configuration.
    pub connection: KafkaConnectionConfig,
    /// Uplink mappings (Gateway -> Kafka), by `NorthwardData` kind.
    #[serde(default)]
    pub uplink: UplinkConfig,
    /// Downlink mappings (Kafka -> Gateway).
    #[serde(default)]
    pub downlink: DownlinkConfig,
}

impl PluginConfig for KafkaPluginConfig {}

/// Kafka transport configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KafkaConnectionConfig {
    /// Comma-separated list of broker addresses.
    ///
    /// Example: `127.0.0.1:9092` or `kafka-1:9092,kafka-2:9092`.
    pub bootstrap_servers: String,
    /// Optional Kafka client.id.
    ///
    /// If not set, the plugin will generate a stable client id derived from `app_id`.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Security options: plaintext/ssl/sasl_plaintext/sasl_ssl.
    #[serde(default)]
    pub security: KafkaSecurityConfig,
}

/// Kafka security configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KafkaSecurityConfig {
    /// Transport protocol.
    #[serde(default)]
    pub protocol: KafkaSecurityProtocol,
    /// TLS options (used when `protocol` is `ssl` or `sasl_ssl`).
    #[serde(default)]
    pub tls: Option<KafkaTlsConfig>,
    /// SASL options (used when `protocol` is `sasl_plaintext` or `sasl_ssl`).
    #[serde(default)]
    pub sasl: Option<KafkaSaslConfig>,
}

/// Supported Kafka security protocols.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KafkaSecurityProtocol {
    /// No transport security.
    #[default]
    Plaintext,
    /// TLS transport (SSL).
    Ssl,
    /// SASL authentication over plaintext.
    SaslPlaintext,
    /// SASL authentication over TLS.
    SaslSsl,
}

/// Kafka TLS configuration (mapped to librdkafka `ssl.*` options).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KafkaTlsConfig {
    /// Path to CA certificate bundle.
    #[serde(default)]
    pub ca_location: Option<String>,
    /// Path to client certificate (PEM).
    #[serde(default)]
    pub certificate_location: Option<String>,
    /// Path to client private key (PEM).
    #[serde(default)]
    pub key_location: Option<String>,
    /// Password for private key (if encrypted).
    #[serde(default)]
    pub key_password: Option<String>,
    /// TLS hostname verification algorithm (librdkafka: `ssl.endpoint.identification.algorithm`).
    ///
    /// Typical values: `https` (enable) or empty string (disable).
    #[serde(default)]
    pub endpoint_identification_algorithm: Option<String>,
}

/// Kafka SASL configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KafkaSaslConfig {
    /// SASL mechanism.
    #[serde(default)]
    pub mechanism: KafkaSaslMechanism,
    /// SASL username.
    pub username: String,
    /// SASL password.
    pub password: String,
}

/// Supported SASL mechanisms.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KafkaSaslMechanism {
    /// SASL/PLAIN.
    #[default]
    Plain,
    /// SASL/SCRAM-SHA-256.
    ScramSha256,
    /// SASL/SCRAM-SHA-512.
    ScramSha512,
}

/// Producer configuration (mapped to librdkafka producer options).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KafkaProducerConfig {
    /// Enable idempotent producer for stronger delivery guarantees.
    ///
    /// When enabled, the plugin will also apply safe defaults to align with Kafka requirements.
    #[serde(default = "KafkaProducerConfig::default_enable_idempotence")]
    pub enable_idempotence: bool,
    /// Acks policy.
    #[serde(default)]
    pub acks: KafkaAcks,
    /// Compression algorithm.
    #[serde(default)]
    pub compression: KafkaCompression,
    /// Linger time for batching (ms).
    #[serde(default = "KafkaProducerConfig::default_linger_ms")]
    pub linger_ms: u32,
    /// Maximum number of messages in a batch (`batch.num.messages`).
    #[serde(default = "KafkaProducerConfig::default_batch_num_messages")]
    pub batch_num_messages: u32,
    /// Maximum batch size in bytes (`batch.size`).
    #[serde(default = "KafkaProducerConfig::default_batch_size_bytes")]
    pub batch_size_bytes: u32,
    /// Message timeout (ms) (`message.timeout.ms`).
    #[serde(default = "KafkaProducerConfig::default_message_timeout_ms")]
    pub message_timeout_ms: u32,
    /// Request timeout (ms) (`request.timeout.ms`).
    #[serde(default = "KafkaProducerConfig::default_request_timeout_ms")]
    pub request_timeout_ms: u32,
    /// Max in-flight requests per connection (`max.in.flight.requests.per.connection`).
    ///
    /// This can affect ordering guarantees when retries happen.
    #[serde(default = "KafkaProducerConfig::default_max_inflight")]
    pub max_inflight: u32,
}

impl KafkaProducerConfig {
    #[inline]
    fn default_enable_idempotence() -> bool {
        true
    }

    #[inline]
    fn default_linger_ms() -> u32 {
        5
    }

    #[inline]
    fn default_batch_num_messages() -> u32 {
        1000
    }

    #[inline]
    fn default_batch_size_bytes() -> u32 {
        // 128 KiB
        128 * 1024
    }

    #[inline]
    fn default_message_timeout_ms() -> u32 {
        30_000
    }

    #[inline]
    fn default_request_timeout_ms() -> u32 {
        10_000
    }

    #[inline]
    fn default_max_inflight() -> u32 {
        5
    }
}

impl Default for KafkaProducerConfig {
    fn default() -> Self {
        Self {
            enable_idempotence: KafkaProducerConfig::default_enable_idempotence(),
            acks: KafkaAcks::default(),
            compression: KafkaCompression::default(),
            linger_ms: KafkaProducerConfig::default_linger_ms(),
            batch_num_messages: KafkaProducerConfig::default_batch_num_messages(),
            batch_size_bytes: KafkaProducerConfig::default_batch_size_bytes(),
            message_timeout_ms: KafkaProducerConfig::default_message_timeout_ms(),
            request_timeout_ms: KafkaProducerConfig::default_request_timeout_ms(),
            max_inflight: KafkaProducerConfig::default_max_inflight(),
        }
    }
}

/// Acks policy for Kafka producer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KafkaAcks {
    /// `acks=0`
    None,
    /// `acks=1`
    One,
    /// `acks=all`
    #[default]
    All,
}

/// Compression for Kafka producer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KafkaCompression {
    None,
    Gzip,
    Snappy,
    #[default]
    Lz4,
    Zstd,
}

/// Default `true` helper for config toggles.
#[inline]
fn default_enabled_true() -> bool {
    true
}

// ===== mappings =====

/// Uplink mapping configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UplinkConfig {
    /// Master switch for uplink mappings.
    ///
    /// NOTE: Defaults to `true` for backward compatibility.
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    /// Producer settings for uplink publish.
    #[serde(default)]
    pub producer: KafkaProducerConfig,
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
            producer: KafkaProducerConfig::default(),
            device_connected: EventUplink::default_device_connected(),
            device_disconnected: EventUplink::default_device_disconnected(),
            telemetry: EventUplink::default_telemetry(),
            attributes: EventUplink::default_attributes(),
        }
    }
}

/// Uplink mapping slot for one `NorthwardData` kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventUplink {
    #[serde(default)]
    pub enabled: bool,
    /// Topic template for uplink publish.
    #[serde(default = "EventUplink::default_uplink_topic")]
    pub topic: String,
    /// Message key template (Kafka record key).
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
        "ng.uplink.{{event_kind}}.{{device_name}}".to_string()
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

/// Downlink (Kafka -> Gateway).
///
/// We wrap the SDK downlink config to customize default topics for Kafka.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownlinkConfig {
    /// Master switch for downlink subscription and mappings.
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    /// Downlink mappings (Kafka -> Gateway), by `NorthwardEvent` kind.
    #[serde(default = "DownlinkConfig::default_write_point")]
    pub write_point: EventDownlink,
    #[serde(default = "DownlinkConfig::default_command_received")]
    pub command_received: EventDownlink,
    #[serde(default = "DownlinkConfig::default_rpc_response_received")]
    pub rpc_response_received: EventDownlink,
}

impl Default for DownlinkConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            write_point: DownlinkConfig::default_write_point(),
            command_received: DownlinkConfig::default_command_received(),
            rpc_response_received: DownlinkConfig::default_rpc_response_received(),
        }
    }
}

impl DownlinkConfig {
    fn default_write_point() -> EventDownlink {
        EventDownlink {
            topic: "ng.downlink".to_string(),
            ..Default::default()
        }
    }

    fn default_command_received() -> EventDownlink {
        EventDownlink {
            topic: "ng.downlink".to_string(),
            ..Default::default()
        }
    }

    fn default_rpc_response_received() -> EventDownlink {
        EventDownlink {
            topic: "ng.downlink".to_string(),
            ..Default::default()
        }
    }
}
