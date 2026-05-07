//! Cross-FFI contract for the OPC UA Server northward plugin.
//!
//! This module defines the **stable, versioned, ABI-safe payload schema** used
//! by the gateway host and the dynamically loaded `opcua-server` plugin to
//! exchange control-plane data via `Plugin::invoke_capability`.
//!
//! # Why JSON / `serde_json::Value`
//! Capability payloads are routed through `Plugin::invoke_capability(&str, Value)`
//! to keep the FFI surface free of trait-object lifetime / vtable concerns.
//! Latency cost of JSON encoding is irrelevant on this control-plane path
//! (single-shot per export request, never per-telemetry).
//!
//! # Source-of-truth boundary
//! All OPC UA wire-format derivations (NodeId construction, BrowsePath,
//! `AccessLevel` flag rendering, type mapping, advertised endpoint composition,
//! certificate management) live **inside the plugin crate**
//! (`ng-plugin-opcua-server`). The host consumes only the rendered strings /
//! pre-computed structs surfaced below; it never reproduces protocol-specific
//! logic. This boundary keeps the plugin solely responsible for any OPC UA
//! spec evolution.
//!
//! # Versioning
//! Capability identifiers are URI-shaped and versioned (`...:v1`). Adding new
//! variants to the `op` enums is a backward-compatible change as long as the
//! plugin advertises an exact match for the requested capability id; breaking
//! changes MUST bump to a new capability id (`...:v2`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Stable capability identifier for the OPC UA Server runtime inspector v1.
///
/// Format: `ng:<plane>:<plugin-type>:<capability>:<version>`.
pub const CAPABILITY_INSPECTOR_V1: &str = "ng:northward:opcua-server:inspector:v1";

/// OPC UA Server inspector request payload (schema v1).
///
/// The `op` discriminant keeps the door open for additional inspector
/// operations (e.g. partial diagnostics) without bumping the schema version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum InspectorRequestV1 {
    /// Snapshot the materialized AddressSpace plus current server metadata.
    Snapshot,
}

/// OPC UA Server inspector response payload (schema v1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum InspectorResponseV1 {
    /// Snapshot reply, paired with the request `Snapshot` op.
    Snapshot(OpcuaServerRuntimeSnapshot),
}

/// Server-level metadata + materialized node bindings reported to the host.
///
/// # Determinism
/// `materialized` is sorted by `(channel_name, device_name, point_key)` so
/// successive snapshots produce stable export bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpcuaServerRuntimeSnapshot {
    /// Effective namespace index registered into the OPC UA address space.
    pub namespace_index: u16,
    /// Namespace URI configured for gateway point nodes.
    pub namespace_uri: String,
    /// OPC UA application URI advertised on the endpoint.
    pub application_uri: String,
    /// OPC UA product URI advertised on the endpoint.
    pub product_uri: String,
    /// Local TCP socket bind address (verbatim from plugin config; may be a
    /// wildcard such as `0.0.0.0:4840`).
    pub bind_addr: String,
    /// Validated advertised endpoint URLs the server publishes via discovery.
    /// Hosts MUST NOT re-derive these.
    pub advertised_endpoints: Vec<String>,
    /// Self-signed application instance certificate summary.
    ///
    /// `None` only when PKI bring-up has not yet completed (e.g. inspector
    /// invoked while the supervisor is still in the connect attempt loop and
    /// no session has published yet). Hosts can render this as a single
    /// "PKI not ready" indicator.
    pub cert_summary: Option<OpcuaServerCertSummary>,
    /// Materialized point bindings (already inserted into the AddressSpace).
    pub materialized: Vec<MaterializedNode>,
}

/// Operator-visible summary of the live application instance certificate.
///
/// All fields are pre-rendered strings / typed timestamps so consumers don't
/// need to depend on any X.509 / OPC UA crate to render them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpcuaServerCertSummary {
    /// SHA-1 hex thumbprint (matches what KEPServerEX / UaExpert display).
    pub thumbprint_hex: String,
    /// X.500 Common Name (typically the gateway application name).
    pub common_name: String,
    /// First URI SAN; equals `application_uri` for a healthy certificate.
    pub san_uri: String,
    /// DNS / hostname `subjectAltName` entries.
    pub san_hostnames: Vec<String>,
    /// IP-literal `subjectAltName` entries.
    pub san_ips: Vec<String>,
    /// Certificate `notBefore` (validity start).
    pub not_before: DateTime<Utc>,
    /// Certificate `notAfter` (validity end).
    pub not_after: DateTime<Utc>,
    /// Days from `Utc::now()` to `not_after` (negative when expired).
    pub days_to_expiry: i64,
    /// One of `"healthy"`, `"expiring"`, `"expired"`.
    pub health: String,
}

/// Self-contained binding view for a single materialized OPC UA point.
///
/// # Self-contained contract
/// Every field required to render an OPC UA-flavoured table row (Excel, CSV,
/// API response) is included. The host MUST NOT attempt to enrich, recompute,
/// or reformat any field — the plugin is the canonical authority for OPC UA
/// semantics.
///
/// # Field rendering policy
/// Enum-valued fields (`point_type`, `access_mode`, `wire_data_type`,
/// `logical_data_type`) are pre-rendered to stable, lower-case snake-case
/// strings so consumers stay decoupled from gateway internal `serde_repr`
/// numeric encodings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterializedNode {
    /// Gateway point identifier (primary key in the southward runtime index).
    pub point_id: i32,

    // --- Gateway entity coordinates ---------------------------------------
    /// Channel display name.
    pub channel_name: String,
    /// Device display name.
    pub device_name: String,
    /// Stable point key inside the device.
    pub point_key: String,
    /// Point display name shown to operators.
    pub point_name: String,
    /// Optional human-readable description.
    pub description: Option<String>,

    // --- Gateway semantics (rendered as stable strings) -------------------
    /// Stable label for the data point category (`telemetry`/`attribute`).
    pub point_type: String,
    /// Stable label for the gateway access mode (`read`/`write`/`read_write`).
    pub access_mode: String,
    /// Stable label for the persisted wire data type (`float32`/`int64`/...).
    pub wire_data_type: String,
    /// Stable label for the logical (post-transform) data type.
    pub logical_data_type: String,

    // --- OPC UA wire format (plugin-owned) --------------------------------
    /// Full OPC UA NodeId, e.g. `ns=2;s=通道一/1号温湿度计/湿度`.
    /// Identifier segments are full UTF-8 with a `／` (U+FF0F) escape applied
    /// to literal `/` inside any segment.
    pub node_id: String,
    /// BrowsePath shown in OPC UA browser tools, rooted at `Objects`.
    pub browse_path: String,
    /// OPC UA built-in data type name (e.g. `Float`, `Int64`).
    pub opcua_data_type: String,
    /// Human-readable OPC UA `AccessLevel` flag set
    /// (e.g. `CurrentRead | CurrentWrite`).
    pub opcua_access_level: String,

    // --- Engineering metadata --------------------------------------------
    /// Engineering unit, if configured.
    pub unit: Option<String>,
    /// Minimum engineering value, if configured.
    pub min_value: Option<f64>,
    /// Maximum engineering value, if configured.
    pub max_value: Option<f64>,

    // --- Transform parameters --------------------------------------------
    /// Transform scale factor (`None` if identity / unset).
    pub transform_scale: Option<f64>,
    /// Transform offset (`None` if identity / unset).
    pub transform_offset: Option<f64>,
    /// Transform negate flag (always meaningful, default `false`).
    pub transform_negate: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cert() -> OpcuaServerCertSummary {
        OpcuaServerCertSummary {
            thumbprint_hex: "deadbeef".into(),
            common_name: "NG-Gateway".into(),
            san_uri: "urn:ng:opcua-server".into(),
            san_hostnames: vec!["gateway.local".into()],
            san_ips: vec!["192.168.1.10".into()],
            not_before: Utc::now(),
            not_after: Utc::now(),
            days_to_expiry: 365,
            health: "healthy".into(),
        }
    }

    #[test]
    fn capability_id_is_versioned() {
        assert!(CAPABILITY_INSPECTOR_V1.starts_with("ng:northward:opcua-server:inspector:"));
        assert!(CAPABILITY_INSPECTOR_V1.ends_with(":v1"));
    }

    #[test]
    fn inspector_request_roundtrips() {
        let value = serde_json::to_value(InspectorRequestV1::Snapshot).unwrap();
        let decoded: InspectorRequestV1 = serde_json::from_value(value).unwrap();
        assert!(matches!(decoded, InspectorRequestV1::Snapshot));
    }

    #[test]
    fn inspector_response_roundtrips() {
        let response = InspectorResponseV1::Snapshot(OpcuaServerRuntimeSnapshot {
            namespace_index: 1,
            namespace_uri: "urn:test".into(),
            application_uri: "urn:test:app".into(),
            product_uri: "urn:test:product".into(),
            bind_addr: "0.0.0.0:4840".into(),
            advertised_endpoints: vec!["opc.tcp://gateway.local:4840/".into()],
            cert_summary: Some(sample_cert()),
            materialized: vec![MaterializedNode {
                point_id: 1,
                channel_name: "ch".into(),
                device_name: "dev".into(),
                point_key: "key".into(),
                point_name: "Name".into(),
                description: Some("d".into()),
                point_type: "telemetry".into(),
                access_mode: "read".into(),
                wire_data_type: "float32".into(),
                logical_data_type: "float32".into(),
                node_id: "ns=1;s=ch/dev/key".into(),
                browse_path: "/Objects/NG-Gateway/ch/dev/key".into(),
                opcua_data_type: "Float".into(),
                opcua_access_level: "CurrentRead".into(),
                unit: Some("℃".into()),
                min_value: Some(-40.0),
                max_value: Some(120.0),
                transform_scale: Some(1.0),
                transform_offset: Some(0.0),
                transform_negate: false,
            }],
        });
        let value = serde_json::to_value(&response).unwrap();
        let decoded: InspectorResponseV1 = serde_json::from_value(value).unwrap();
        match decoded {
            InspectorResponseV1::Snapshot(s) => {
                assert_eq!(s.namespace_index, 1);
                assert_eq!(s.advertised_endpoints.len(), 1);
                assert_eq!(s.materialized.len(), 1);
                assert_eq!(s.materialized[0].node_id, "ns=1;s=ch/dev/key");
                assert!(s.cert_summary.is_some());
            }
        }
    }

    #[test]
    fn snapshot_serialises_chinese_node_id_verbatim() {
        let snap = OpcuaServerRuntimeSnapshot {
            namespace_index: 2,
            namespace_uri: "urn:test".into(),
            application_uri: "urn:test:app".into(),
            product_uri: "urn:test:product".into(),
            bind_addr: "0.0.0.0:4840".into(),
            advertised_endpoints: vec!["opc.tcp://gateway.local:4840/".into()],
            cert_summary: None,
            materialized: vec![MaterializedNode {
                point_id: 1,
                channel_name: "通道一".into(),
                device_name: "1号温湿度计".into(),
                point_key: "湿度".into(),
                point_name: "湿度".into(),
                description: None,
                point_type: "telemetry".into(),
                access_mode: "read".into(),
                wire_data_type: "float32".into(),
                logical_data_type: "float32".into(),
                node_id: "ns=2;s=通道一/1号温湿度计/湿度".into(),
                browse_path: "/Objects/NG-Gateway/通道一/1号温湿度计/湿度".into(),
                opcua_data_type: "Float".into(),
                opcua_access_level: "CurrentRead".into(),
                unit: None,
                min_value: None,
                max_value: None,
                transform_scale: None,
                transform_offset: None,
                transform_negate: false,
            }],
        };
        let json = serde_json::to_string(&snap).unwrap();
        // Use 'as &str' to silence ambiguous numeric type lints.
        assert!(json.contains(r#""node_id":"ns=2;s=通道一/1号温湿度计/湿度""#));
    }
}
