//! OPC UA Server northward plugin configuration.
//!
//! # Production-defaulted
//! - Security defaults to `SignAndEncrypt + Basic256Sha256` plus an anonymous
//!   `None` endpoint for first-run development convenience.
//! - Update queue defaults favor real-time freshness (`discard_oldest`).
//! - The TCP bind address is decoupled from the advertised endpoint URLs:
//!   * `bind_addr` controls the local socket (typically `0.0.0.0:port` for
//!     multi-interface listening, both bare-metal and Docker host).
//!   * `advertised_endpoints` is the **client-facing** URL list embedded in
//!     `GetEndpointsResponse` / `CreateSessionResponse`. Strict OPC UA clients
//!     (KEPServerEX, UaExpert) reject any endpoint whose host is a wildcard
//!     such as `0.0.0.0` or `::`, so this field MUST contain at least one
//!     concrete hostname or IP that the client can reach.
//! - Certificate lifecycle: the plugin self-manages the application instance
//!   certificate (see `pki` module). Configuration drift (changes to
//!   `application_uri` or the `advertised_endpoints` host list) automatically
//!   triggers regeneration with the old certificate archived.

use ng_gateway_sdk::PluginConfig;
use serde::{Deserialize, Deserializer, Serialize};

/// OPC UA Server northward plugin configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpcuaServerPluginConfig {
    /// TCP socket bind address in `host:port` form (e.g. `0.0.0.0:4840`).
    ///
    /// Wildcards `0.0.0.0` / `[::]` are explicitly allowed here so the same
    /// gateway image works in:
    /// - bare-metal `.deb` deployments (multi-interface listening),
    /// - Docker `--network host` (multi-interface),
    /// - Docker bridge (`-p 4840:4840`, container internal still 0.0.0.0).
    ///
    /// The advertised endpoint URL is independently configured via
    /// `advertised_endpoints` and never derived from this field.
    #[serde(default = "OpcuaServerPluginConfig::default_bind_addr")]
    pub bind_addr: String,

    /// Client-facing OPC UA endpoint URLs the server publishes in discovery.
    ///
    /// **Required, non-empty.** Each entry MUST be a valid `opc.tcp://host[:port][/path]`
    /// URL with a concrete host. Wildcards (`0.0.0.0`, `::`) are rejected at
    /// startup because GetEndpoints would otherwise advertise unreachable URLs
    /// that strict OPC UA clients refuse.
    ///
    /// This list is the **single source of truth** for both:
    /// - `discovery_urls` published via `GetEndpointsResponse`
    /// - the certificate's `subjectAltName` host list (each entry's host is
    ///   injected as a DNS / IP SAN automatically)
    ///
    /// The first entry seeds `ServerBuilder.host()` / `port()` (used internally
    /// by `async-opcua-server` to render its `EndpointDescription.endpointUrl`).
    /// To cover multi-homed reachability (LAN IP + DNS + alias, multiple NICs,
    /// reverse-proxy hostname, etc.) simply add every URL clients will use.
    ///
    /// # Typical filling
    /// - bare-metal: `["opc.tcp://192.168.1.10:4840/", "opc.tcp://gateway.local:4840/"]`
    /// - Docker bridge `-p 4840:4840`: `["opc.tcp://<host_ip>:4840/"]`
    /// - K8s NodePort 30840: `["opc.tcp://<node_ip>:30840/"]`
    #[serde(deserialize_with = "deserialize_string_array_lenient")]
    pub advertised_endpoints: Vec<String>,

    /// OPC UA application URI (must be stable), e.g. `urn:ng:opcua-server`.
    pub application_uri: String,

    /// OPC UA product URI, e.g. `urn:ng:opcua-server`.
    pub product_uri: String,

    /// Namespace URI for NG-Gateway points; registered as `ns=<discovered>` at
    /// runtime. The effective index is reported via the inspector snapshot.
    pub namespace_uri: String,

    /// Trusted client application instance certificates.
    ///
    /// Each entry can be either:
    /// - PEM (with `-----BEGIN CERTIFICATE-----` markers), or
    /// - base64-encoded DER (no markers).
    ///
    /// Materialized into the plugin PKI trust store under `pki/.../trusted/`
    /// at startup so `async-opcua-server` validates secure channels using its
    /// native logic.
    #[serde(default, deserialize_with = "deserialize_string_array_lenient")]
    pub trusted_client_certs: Vec<String>,

    /// Update queue capacity (batches).
    #[serde(default = "OpcuaServerPluginConfig::default_update_queue_capacity")]
    pub update_queue_capacity: usize,

    /// Back-pressure drop policy when queue is full.
    #[serde(default)]
    pub drop_policy: DropPolicy,

    /// Overall timeout for a single OPC UA write request (ms).
    ///
    /// Bounds: enqueue (gateway per-channel serialization) + southward driver write.
    #[serde(default = "OpcuaServerPluginConfig::default_write_timeout_ms")]
    pub write_timeout_ms: u64,

    /// Days-to-expiry threshold below which the certificate-expiry monitor
    /// emits a `Warning` log; below 3 days it escalates to `Critical`.
    #[serde(default = "OpcuaServerPluginConfig::default_cert_expiry_warn_days")]
    pub cert_expiry_warn_days: u32,
}

impl OpcuaServerPluginConfig {
    fn default_bind_addr() -> String {
        "0.0.0.0:4840".to_string()
    }

    fn default_update_queue_capacity() -> usize {
        10_000
    }
    fn default_write_timeout_ms() -> u64 {
        5_000
    }
    fn default_cert_expiry_warn_days() -> u32 {
        30
    }
}

impl Default for OpcuaServerPluginConfig {
    fn default() -> Self {
        Self {
            bind_addr: Self::default_bind_addr(),
            // Intentionally empty: an empty list is rejected at startup with a
            // clear error guiding the operator to fill in concrete endpoints.
            advertised_endpoints: Vec::new(),
            // IMPORTANT:
            // Keep `application_uri` distinct from `namespace_uri`.
            // `async-opcua-server` diagnostics node manager uses `application_uri`
            // as its namespace, and if it matches our data namespace it can
            // intercept requests depending on node manager ordering.
            application_uri: "urn:ng:opcua-server".to_string(),
            product_uri: "urn:ng:opcua-server".to_string(),
            namespace_uri: "urn:ng:ng-gateway".to_string(),
            trusted_client_certs: Vec::new(),
            update_queue_capacity: Self::default_update_queue_capacity(),
            drop_policy: DropPolicy::DiscardOldest,
            write_timeout_ms: Self::default_write_timeout_ms(),
            cert_expiry_warn_days: Self::default_cert_expiry_warn_days(),
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

/// Deserialize a `Vec<String>` field that tolerates a JSON-encoded string in
/// addition to a real array.
///
/// # Why we need this
/// The dynamic-schema UI renders `UiDataType::Any` fields with `json-editor-vue`,
/// whose upstream default `stringified: true` causes the form value to be
/// shipped as a JSON-encoded string (e.g. `"[\"opc.tcp://...\"]"`) rather than
/// a real array. The frontend has been patched to disable that default, but
/// the same plugin config might also be loaded from:
/// - operator-supplied YAML / JSON files where someone hand-wrote a string,
/// - upstream tools that don't share our dynamic-schema patch,
/// - older saved configs persisted before the frontend patch.
///
/// Accepting both shapes makes the config robust against any of these without
/// surfacing a confusing "invalid type: string … expected a sequence" error.
///
/// # Accepted inputs
/// - `["opc.tcp://...", "opc.tcp://..."]` — canonical array
/// - `"[\"opc.tcp://...\"]"`              — JSON-array-as-string
/// - `null`                               — equivalent to empty array
fn deserialize_string_array_lenient<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .map(|v| match v {
                serde_json::Value::String(s) => Ok(s),
                other => Err(D::Error::custom(format!(
                    "expected string element, got {}",
                    json_kind(&other)
                ))),
            })
            .collect(),
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(Vec::new());
            }
            let parsed: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
                D::Error::custom(format!(
                    "expected an array or a JSON-array-encoded string, got {trimmed:?}: {e}"
                ))
            })?;
            match parsed {
                serde_json::Value::Array(arr) => arr
                    .into_iter()
                    .map(|v| match v {
                        serde_json::Value::String(s) => Ok(s),
                        other => Err(D::Error::custom(format!(
                            "expected string element inside JSON-array-encoded string, got {}",
                            json_kind(&other)
                        ))),
                    })
                    .collect(),
                other => Err(D::Error::custom(format!(
                    "expected JSON-array-encoded string, got {}",
                    json_kind(&other)
                ))),
            }
        }
        other => Err(D::Error::custom(format!(
            "expected an array or a JSON-array-encoded string, got {}",
            json_kind(&other)
        ))),
    }
}

#[inline]
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_accepts_canonical_array() {
        let json = serde_json::json!({
            "bind_addr": "0.0.0.0:4840",
            "advertised_endpoints": ["opc.tcp://192.168.1.10:4840/"],
            "application_uri": "urn:test",
            "product_uri": "urn:test",
            "namespace_uri": "urn:test:ns",
        });
        let cfg: OpcuaServerPluginConfig = serde_json::from_value(json).unwrap();
        assert_eq!(
            cfg.advertised_endpoints,
            vec!["opc.tcp://192.168.1.10:4840/".to_string()]
        );
    }

    #[test]
    fn config_accepts_stringified_array_for_advertised_endpoints() {
        let json = serde_json::json!({
            "bind_addr": "0.0.0.0:4840",
            "advertised_endpoints": "[\n  \"opc.tcp://192.168.66.124:4840/\"\n]",
            "application_uri": "urn:test",
            "product_uri": "urn:test",
            "namespace_uri": "urn:test:ns",
        });
        let cfg: OpcuaServerPluginConfig = serde_json::from_value(json).unwrap();
        assert_eq!(
            cfg.advertised_endpoints,
            vec!["opc.tcp://192.168.66.124:4840/".to_string()]
        );
    }

    #[test]
    fn config_accepts_stringified_array_for_trusted_client_certs() {
        let json = serde_json::json!({
            "bind_addr": "0.0.0.0:4840",
            "advertised_endpoints": ["opc.tcp://gateway.local:4840/"],
            "trusted_client_certs": "[\"-----BEGIN CERTIFICATE-----\\n...\\n-----END CERTIFICATE-----\"]",
            "application_uri": "urn:test",
            "product_uri": "urn:test",
            "namespace_uri": "urn:test:ns",
        });
        let cfg: OpcuaServerPluginConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg.trusted_client_certs.len(), 1);
        assert!(cfg.trusted_client_certs[0].contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn config_accepts_empty_string_as_empty_array() {
        let json = serde_json::json!({
            "bind_addr": "0.0.0.0:4840",
            "advertised_endpoints": "",
            "application_uri": "urn:test",
            "product_uri": "urn:test",
            "namespace_uri": "urn:test:ns",
        });
        let cfg: OpcuaServerPluginConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.advertised_endpoints.is_empty());
    }

    #[test]
    fn config_accepts_null_for_optional_arrays() {
        let json = serde_json::json!({
            "bind_addr": "0.0.0.0:4840",
            "advertised_endpoints": ["opc.tcp://gateway.local:4840/"],
            "trusted_client_certs": null,
            "application_uri": "urn:test",
            "product_uri": "urn:test",
            "namespace_uri": "urn:test:ns",
        });
        let cfg: OpcuaServerPluginConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.trusted_client_certs.is_empty());
    }

    #[test]
    fn config_rejects_non_string_elements_in_array() {
        let json = serde_json::json!({
            "bind_addr": "0.0.0.0:4840",
            "advertised_endpoints": [42],
            "application_uri": "urn:test",
            "product_uri": "urn:test",
            "namespace_uri": "urn:test:ns",
        });
        let err = serde_json::from_value::<OpcuaServerPluginConfig>(json).unwrap_err();
        assert!(err.to_string().contains("expected string"));
    }

    #[test]
    fn config_rejects_arbitrary_non_json_string() {
        let json = serde_json::json!({
            "bind_addr": "0.0.0.0:4840",
            "advertised_endpoints": "not json",
            "application_uri": "urn:test",
            "product_uri": "urn:test",
            "namespace_uri": "urn:test:ns",
        });
        let err = serde_json::from_value::<OpcuaServerPluginConfig>(json).unwrap_err();
        assert!(err.to_string().contains("JSON-array-encoded string"));
    }
}
