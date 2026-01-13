use ng_gateway_sdk::PluginConfig;
use serde::{Deserialize, Serialize};

/// OPC UA Server northward plugin configuration.
///
/// This config is intentionally "production defaulted":
/// - security defaults to SignAndEncrypt + Basic256Sha256
/// - update queue defaults favor real-time freshness (discard_oldest)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpcuaServerPluginConfig {
    /// Hostname / IP to bind, e.g. "0.0.0.0"
    pub host: String,

    /// Port to bind, e.g. 4840
    pub port: u16,

    /// OPC UA application URI (must be stable), e.g. "urn:ng:ng-gateway"
    pub application_uri: String,

    /// OPC UA product URI, e.g. "urn:ng:opcua-server"
    pub product_uri: String,

    /// Namespace URI for NG-Gateway mapping. The plugin will register this as ns=1.
    pub namespace_uri: String,

    /// Trusted client application instance certificates.
    ///
    /// Each entry can be either:
    /// - PEM (including the BEGIN/END CERTIFICATE markers), or
    /// - base64-encoded DER (no markers).
    ///
    /// These certificates will be materialized into the plugin PKI trust store
    /// (under `pki/.../trusted/`) on startup so that the server can validate
    /// secure channels/sessions using the library's native logic.
    #[serde(default)]
    pub trusted_client_certs: Vec<String>,

    /// Update queue capacity (batches).
    #[serde(default = "OpcuaServerPluginConfig::default_update_queue_capacity")]
    pub update_queue_capacity: usize,

    /// Back-pressure drop policy when queue is full.
    #[serde(default)]
    pub drop_policy: DropPolicy,

    /// Overall timeout for a single OPC UA write request (ms).
    ///
    /// This bounds: enqueue (gateway per-channel serialization) + southward driver write.
    #[serde(default = "OpcuaServerPluginConfig::default_write_timeout_ms")]
    pub write_timeout_ms: u64,
}

impl OpcuaServerPluginConfig {
    fn default_update_queue_capacity() -> usize {
        10_000
    }
    fn default_write_timeout_ms() -> u64 {
        5_000
    }
}

impl Default for OpcuaServerPluginConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 4840,
            // IMPORTANT:
            // Keep `application_uri` distinct from `namespace_uri`.
            // `async-opcua-server` diagnostics node manager uses `application_uri` as its namespace,
            // and if it matches our data namespace it can intercept requests depending on
            // node manager ordering.
            application_uri: "urn:ng:opcua-server".to_string(),
            product_uri: "urn:ng:opcua-server".to_string(),
            namespace_uri: "urn:ng:ng-gateway".to_string(),
            trusted_client_certs: Vec::new(),
            update_queue_capacity: OpcuaServerPluginConfig::default_update_queue_capacity(),
            drop_policy: DropPolicy::DiscardOldest,
            write_timeout_ms: OpcuaServerPluginConfig::default_write_timeout_ms(),
        }
    }
}

impl PluginConfig for OpcuaServerPluginConfig {}

#[derive(Default, Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropPolicy {
    #[default]
    DiscardOldest,
    DiscardNewest,
    BlockWithTimeout,
}
