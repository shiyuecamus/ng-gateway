use ng_gateway_sdk::PluginConfig;
use rumqttc::QoS;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

/// ThingsBoard plugin configuration
///
/// This configuration supports multiple connection modes and communication settings.
/// It's designed to handle various authentication methods and provision scenarios.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThingsBoardPluginConfig {
    /// Connection configuration (authentication and provision)
    pub connection: ConnectionConfig,
    /// Communication configuration (MQTT settings)
    pub communication: CommunicationConfig,
}

impl PluginConfig for ThingsBoardPluginConfig {}

/// Connection configuration for ThingsBoard
///
/// Supports two main modes:
/// 1. **Direct Connection**: Use pre-configured credentials (None, UsernamePassword, Token, X509)
/// 2. **Provision**: Dynamically provision device and obtain credentials
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ConnectionConfig {
    /// Direct connection without authentication (not recommended for production)
    None {
        /// MQTT broker host
        host: String,
        /// MQTT broker port
        port: u16,
        /// Optional client ID (auto-generated if not provided)
        #[serde(skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
    },

    /// Direct connection with username/password authentication
    UsernamePassword {
        /// MQTT broker host
        host: String,
        /// MQTT broker port
        port: u16,
        /// Optional client ID (auto-generated if not provided)
        #[serde(skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        /// MQTT username
        username: String,
        /// MQTT password
        password: String,
    },

    /// Direct connection with access token authentication
    Token {
        /// MQTT broker host
        host: String,
        /// MQTT broker port
        port: u16,
        /// Optional client ID (auto-generated if not provided)
        #[serde(skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        /// ThingsBoard access token
        access_token: String,
    },

    /// Direct connection with X.509 certificate authentication
    X509Certificate {
        /// MQTT broker host
        host: String,
        /// MQTT broker TLS port
        tls_port: u16,
        /// Optional client ID (auto-generated if not provided)
        #[serde(skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        /// Path to client certificate file (PEM format)
        cert_path: String,
        /// Path to client private key file (PEM format)
        private_key_path: String,
    },

    /// Provision mode - dynamically obtain credentials from ThingsBoard
    Provision {
        /// Provision server host
        host: String,
        /// Provision server port (for non-X509 methods)
        port: u16,
        /// Provision server TLS port (for X509 method)
        tls_port: u16,
        /// Single provision request timeout in milliseconds
        timeout_ms: u64,
        /// Maximum provision retry attempts (0 = infinite retries)
        max_retries: u32,
        /// Delay between provision retries in milliseconds
        retry_delay_ms: u64,
        /// Provision device key (from ThingsBoard device profile)
        provision_device_key: String,
        /// Provision device secret (from ThingsBoard device profile)
        provision_device_secret: String,
        /// Provision method (determines credential type to request)
        provision_method: ProvisionMethod,
    },
}

/// Provision method for ThingsBoard device provisioning
///
/// This determines what type of credentials will be requested and received.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProvisionMethod {
    /// Request access token credentials
    AccessToken,
    /// Request MQTT basic auth credentials (username/password)
    MqttBasic,
    /// Request X.509 certificate credentials
    X509Certificate,
}

/// Communication configuration for MQTT
///
/// These settings control how the plugin communicates with ThingsBoard
/// after connection is established.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunicationConfig {
    /// Message format for telemetry and attributes
    #[serde(default = "CommunicationConfig::default_message_format")]
    pub message_format: MessageFormat,
    /// MQTT QoS level (0, 1, or 2)
    #[serde(default = "CommunicationConfig::default_qos")]
    pub qos: u8,
    /// Whether to retain messages
    #[serde(default = "CommunicationConfig::default_retain_messages")]
    pub retain_messages: bool,
    /// MQTT keep-alive interval in seconds
    #[serde(default = "CommunicationConfig::default_keep_alive")]
    pub keep_alive: u16,
    /// MQTT clean session flag
    #[serde(default = "CommunicationConfig::default_clean_session")]
    pub clean_session: bool,
}

impl CommunicationConfig {
    pub fn qos(&self) -> QoS {
        match self.qos {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => QoS::AtLeastOnce,
        }
    }
}

/// Message format for data encoding
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageFormat {
    /// JSON format (human-readable, larger size)
    Json,
    /// Protocol Buffers format (binary, smaller size, faster)
    Protobuf,
}

impl CommunicationConfig {
    // Default value functions for serde
    fn default_message_format() -> MessageFormat {
        MessageFormat::Json
    }

    fn default_qos() -> u8 {
        1
    }

    fn default_retain_messages() -> bool {
        false
    }

    fn default_keep_alive() -> u16 {
        60
    }

    fn default_clean_session() -> bool {
        false
    }
}

impl Default for CommunicationConfig {
    fn default() -> Self {
        Self {
            message_format: CommunicationConfig::default_message_format(),
            qos: CommunicationConfig::default_qos(),
            retain_messages: CommunicationConfig::default_retain_messages(),
            keep_alive: CommunicationConfig::default_keep_alive(),
            clean_session: CommunicationConfig::default_clean_session(),
        }
    }
}

impl ThingsBoardPluginConfig {
    /// Get the MQTT broker host from connection config
    #[inline]
    pub fn host(&self) -> &str {
        match &self.connection {
            ConnectionConfig::None { host, .. }
            | ConnectionConfig::UsernamePassword { host, .. }
            | ConnectionConfig::Token { host, .. }
            | ConnectionConfig::X509Certificate { host, .. }
            | ConnectionConfig::Provision { host, .. } => host,
        }
    }

    /// Get the MQTT broker port from connection config
    #[inline]
    pub fn port(&self) -> u16 {
        match &self.connection {
            ConnectionConfig::None { port, .. }
            | ConnectionConfig::UsernamePassword { port, .. }
            | ConnectionConfig::Token { port, .. } => *port,
            ConnectionConfig::Provision {
                port,
                provision_method,
                tls_port,
                ..
            } => match *provision_method {
                ProvisionMethod::AccessToken => *port,
                ProvisionMethod::MqttBasic => *port,
                ProvisionMethod::X509Certificate => *tls_port,
            },
            ConnectionConfig::X509Certificate { tls_port, .. } => *tls_port,
        }
    }

    /// Check if this configuration requires provision
    #[inline]
    pub fn requires_provision(&self) -> bool {
        matches!(&self.connection, ConnectionConfig::Provision { .. })
    }

    /// Check if this configuration uses TLS
    #[inline]
    pub fn uses_tls(&self) -> bool {
        matches!(&self.connection, ConnectionConfig::X509Certificate { .. })
    }
}
