//! OPC UA Server protocol-specific derivations.
//!
//! This module is the **single source of truth** for all OPC UA Server wire-format
//! decisions made by this plugin:
//! - `NodeId` construction (`ns=N;s=channel/device/point_key`, full UTF-8)
//! - BrowsePath layout under `/Objects/NG-Gateway/{channel}/{device}/{point_key}`
//! - Mapping of gateway logical/wire data types to OPC UA built-in types
//! - Mapping of gateway access modes to OPC UA `AccessLevel` flags
//! - Advertised endpoint URL parsing and validation
//!
//! All derivations are pure functions over `PointMeta` + plugin config and never
//! touch IO. They are intentionally kept private to the plugin and surfaced to the
//! host process only through the cross-FFI inspector DTO defined in
//! `ng_gateway_sdk::northward::opcua_server`.
//!
//! # NodeId encoding contract
//!
//! - Identifier shape: `{channel}/{device}/{point_key}` joined with literal `/`.
//! - Each segment is UTF-8 directly (CJK / accents / spaces / punctuation are
//!   preserved verbatim per OPC UA Part 6 §5.3.1.10 — `UAString` is full Unicode).
//! - The only character substituted in a segment is the separator `/` itself,
//!   which becomes the full-width Unicode equivalent `／` (U+FF0F) so the
//!   identifier remains unambiguously parseable into 3 segments.
//! - C0 / DEL control characters (`U+0000..=U+001F`, `U+007F`) collapse to `_`
//!   to keep `UAString` payloads safe for downstream consumers and logs.
//!
//! Changing the separator, escape rule, or path shape is a wire-format breaking
//! change for existing OPC UA clients (NodeId / BrowsePath bindings) and MUST
//! be paired with a major version bump of the plugin and the
//! `CAPABILITY_INSPECTOR_V*` schema.
//!
//! # Endpoint URL contract
//!
//! `parse_advertised_endpoint` enforces:
//! - scheme `opc.tcp`
//! - non-empty, non-wildcard host (`0.0.0.0` / `::` / `0:0:0:0:0:0:0:0` rejected)
//! - port defaults to `4840` when omitted
//! - path defaults to `/` when missing
//!
//! `validate_advertised_endpoints` additionally requires:
//! - the list is non-empty
//! - all entries parse cleanly
//! - the canonical `host:port/path` triplet is unique across the list

use ng_gateway_sdk::{AccessMode, DataPointType, DataType, NorthwardError, PointMeta};
use opcua::types::NodeId;
use std::collections::HashSet;
use url::Url;

/// Path separator used inside string `NodeId` identifiers and `BrowsePath` segments.
pub(crate) const NODE_ID_PATH_SEPARATOR: char = '/';

/// Full-width replacement injected into a segment when the original text
/// contains the literal path separator.
const NODE_ID_PATH_SEPARATOR_ESCAPED: char = '\u{FF0F}'; // FULLWIDTH SOLIDUS

/// Replacement for ASCII control characters (`U+0000..=U+001F`, `U+007F`).
///
/// These are illegal inside OPC UA `UAString` per Part 6 §5.2.2.4 and would also
/// corrupt log lines / UI rendering; substitute with a single underscore.
const CONTROL_CHAR_REPLACEMENT: char = '_';

/// OPC UA built-in data type names used by NG-Gateway exports.
///
/// These constants match the canonical names defined in
/// `OPC 10000-3` (Address Space Model, §5.4 Data Type Names) so external clients
/// can compare them as opaque strings.
pub mod opcua_data_type {
    pub const BOOLEAN: &str = "Boolean";
    pub const SBYTE: &str = "SByte";
    pub const BYTE: &str = "Byte";
    pub const INT16: &str = "Int16";
    pub const UINT16: &str = "UInt16";
    pub const INT32: &str = "Int32";
    pub const UINT32: &str = "UInt32";
    pub const INT64: &str = "Int64";
    pub const UINT64: &str = "UInt64";
    pub const FLOAT: &str = "Float";
    pub const DOUBLE: &str = "Double";
    pub const STRING: &str = "String";
    pub const BYTE_STRING: &str = "ByteString";
    pub const DATE_TIME: &str = "DateTime";
}

/// Map a gateway `DataType` to the canonical OPC UA built-in type name.
#[inline]
pub fn opcua_data_type_name(data_type: DataType) -> &'static str {
    use opcua_data_type::*;
    match data_type {
        DataType::Boolean => BOOLEAN,
        DataType::Int8 => SBYTE,
        DataType::UInt8 => BYTE,
        DataType::Int16 => INT16,
        DataType::UInt16 => UINT16,
        DataType::Int32 => INT32,
        DataType::UInt32 => UINT32,
        DataType::Int64 => INT64,
        DataType::UInt64 => UINT64,
        DataType::Float32 => FLOAT,
        DataType::Float64 => DOUBLE,
        DataType::String => STRING,
        DataType::Binary => BYTE_STRING,
        DataType::Timestamp => DATE_TIME,
    }
}

/// Render gateway access mode as the human-readable OPC UA `AccessLevel` flag set.
#[inline]
pub fn opcua_access_level_label(access_mode: AccessMode) -> &'static str {
    match access_mode {
        AccessMode::Read => "CurrentRead",
        AccessMode::Write => "CurrentWrite",
        AccessMode::ReadWrite => "CurrentRead | CurrentWrite",
    }
}

/// Render `DataPointType` for inspector responses.
#[inline]
pub fn point_type_label(point_type: DataPointType) -> &'static str {
    match point_type {
        DataPointType::Telemetry => "telemetry",
        DataPointType::Attribute => "attribute",
    }
}

/// Render gateway `AccessMode` for inspector responses.
#[inline]
pub fn access_mode_label(access_mode: AccessMode) -> &'static str {
    match access_mode {
        AccessMode::Read => "read",
        AccessMode::Write => "write",
        AccessMode::ReadWrite => "read_write",
    }
}

/// Render `DataType` for inspector responses (snake_case, stable).
#[inline]
pub fn data_type_label(data_type: DataType) -> &'static str {
    match data_type {
        DataType::Boolean => "boolean",
        DataType::Int8 => "int8",
        DataType::UInt8 => "uint8",
        DataType::Int16 => "int16",
        DataType::UInt16 => "uint16",
        DataType::Int32 => "int32",
        DataType::UInt32 => "uint32",
        DataType::Int64 => "int64",
        DataType::UInt64 => "uint64",
        DataType::Float32 => "float32",
        DataType::Float64 => "float64",
        DataType::String => "string",
        DataType::Binary => "binary",
        DataType::Timestamp => "timestamp",
    }
}

/// Escape a single segment (channel / device / point_key) for use inside
/// the `NodeId` identifier or BrowsePath.
///
/// # Invariants
/// - All non-control / non-separator characters (including CJK, `.`, spaces,
///   accents, punctuation) are preserved verbatim.
/// - The path separator `/` becomes its full-width counterpart `／` (U+FF0F)
///   so the joined identifier remains unambiguously splittable.
/// - C0 and DEL control characters (`U+0000..=U+001F`, `U+007F`) collapse to `_`.
///
/// The escape is lossless when applied to all segments consistently: a client
/// that knows the convention can recover the original segment by replacing
/// `／` back with `/`.
pub(crate) fn escape_segment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch == NODE_ID_PATH_SEPARATOR {
            out.push(NODE_ID_PATH_SEPARATOR_ESCAPED);
        } else if (ch as u32) < 0x20 || ch == '\u{007F}' {
            out.push(CONTROL_CHAR_REPLACEMENT);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Build the dotted/slashed identifier path used inside string NodeIds
/// (without the `ns=N;s=` prefix).
#[inline]
fn make_node_id_identifier(channel: &str, device: &str, point_key: &str) -> String {
    let mut s = String::with_capacity(channel.len() + device.len() + point_key.len() + 2);
    s.push_str(&escape_segment(channel));
    s.push(NODE_ID_PATH_SEPARATOR);
    s.push_str(&escape_segment(device));
    s.push(NODE_ID_PATH_SEPARATOR);
    s.push_str(&escape_segment(point_key));
    s
}

/// Build the canonical OPC UA `NodeId` for a gateway point.
///
/// The identifier carries the full UTF-8 channel / device / point_key, which
/// is exactly the OPC UA contract for `String` NodeIds (UAString = Unicode).
/// No `from_str` round-trip is involved.
#[inline]
pub(crate) fn make_node_id(namespace_index: u16, meta: &PointMeta) -> NodeId {
    let identifier = make_node_id_identifier(
        meta.channel_name.as_ref(),
        meta.device_name.as_ref(),
        meta.point_key.as_ref(),
    );
    NodeId::new(namespace_index, identifier)
}

/// Build the BrowsePath shown to OPC UA browser tools.
///
/// # Format
/// `/Objects/NG-Gateway/{channel}/{device}/{point_key}`. Each segment is run
/// through `escape_segment` so the BrowsePath remains splittable on `/`.
#[inline]
pub(crate) fn make_browse_path(channel: &str, device: &str, point_key: &str) -> String {
    format!(
        "/Objects/NG-Gateway/{}/{}/{}",
        escape_segment(channel),
        escape_segment(device),
        escape_segment(point_key)
    )
}

/// A parsed advertised OPC UA endpoint URL.
///
/// Produced by [`parse_advertised_endpoint`]; consumed by `pki` to compute the
/// certificate SAN list and by `server` to drive `ServerBuilder.host(...)` and
/// `discovery_urls(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointAddr {
    /// Host literal exactly as supplied (hostname or IP, no brackets for IPv6).
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// URL path component (always starts with `/`, defaults to `/`).
    pub path: String,
    /// Original raw URL (kept for log / DTO fidelity).
    pub raw: String,
}

impl EndpointAddr {
    /// Render back to a canonical `opc.tcp://host:port/path` form.
    pub fn canonical(&self) -> String {
        let host_for_url = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("opc.tcp://{}:{}{}", host_for_url, self.port, self.path)
    }
}

/// Default OPC UA TCP port (Part 6 §7.1).
pub const DEFAULT_OPCUA_TCP_PORT: u16 = 4840;

/// Wildcard host literals that are illegal inside an advertised endpoint URL.
const WILDCARD_HOSTS: &[&str] = &["0.0.0.0", "::", "0:0:0:0:0:0:0:0"];

/// Parse and validate a single advertised endpoint URL.
///
/// # Rejected inputs
/// - Empty / whitespace-only string
/// - Non-`opc.tcp` scheme
/// - Wildcard host (`0.0.0.0`, `::`, `0:0:0:0:0:0:0:0`) — no client can dial it
/// - Missing host
pub fn parse_advertised_endpoint(raw: &str) -> Result<EndpointAddr, NorthwardError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(NorthwardError::ConfigurationError {
            message: "advertised endpoint URL must not be empty".to_string(),
        });
    }

    let url = Url::parse(trimmed).map_err(|e| NorthwardError::ConfigurationError {
        message: format!("invalid advertised endpoint URL '{trimmed}': {e}"),
    })?;

    if url.scheme() != "opc.tcp" {
        return Err(NorthwardError::ConfigurationError {
            message: format!(
                "advertised endpoint URL '{trimmed}' must use 'opc.tcp' scheme, got '{}'",
                url.scheme()
            ),
        });
    }

    // `url::Url::host_str` returns IPv6 literals wrapped in brackets
    // (e.g. `[fe80::1]`); strip them so downstream consumers see a bare
    // hostname / IP suitable for direct comparison with `IpAddr::parse` and
    // for use as a `subjectAltName` IP.
    let host_raw = url
        .host_str()
        .ok_or_else(|| NorthwardError::ConfigurationError {
            message: format!("advertised endpoint URL '{trimmed}' has no host"),
        })?;
    let host_str = host_raw
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host_raw)
        .to_string();

    if WILDCARD_HOSTS
        .iter()
        .any(|w| host_str.eq_ignore_ascii_case(w))
    {
        return Err(NorthwardError::ConfigurationError {
            message: format!(
                "advertised endpoint URL '{trimmed}' uses wildcard host '{host_str}'; \
                 specify a concrete hostname or IP that OPC UA clients can dial"
            ),
        });
    }

    let port = url.port().unwrap_or(DEFAULT_OPCUA_TCP_PORT);
    let path = if url.path().is_empty() {
        "/".to_string()
    } else {
        url.path().to_string()
    };

    Ok(EndpointAddr {
        host: host_str,
        port,
        path,
        raw: trimmed.to_string(),
    })
}

/// Validate the entire `advertised_endpoints` list.
///
/// # Guarantees
/// - Returned `Vec` preserves the configured order (the first entry is the
///   primary endpoint used to seed `ServerBuilder.host()` / `port()`).
/// - All entries are unique on `(host, port, path)`.
/// - Returns `Err(ConfigurationError)` for the first violation observed,
///   with a message that names the offending entry.
pub fn validate_advertised_endpoints(
    endpoints: &[String],
) -> Result<Vec<EndpointAddr>, NorthwardError> {
    if endpoints.is_empty() {
        return Err(NorthwardError::ConfigurationError {
            message: "advertised_endpoints must contain at least one OPC UA endpoint URL \
                      (e.g. \"opc.tcp://192.168.1.10:4840/\"); leaving it empty would publish \
                      an unreachable endpoint such as opc.tcp://0.0.0.0:4840/ which strict \
                      OPC UA clients (KEPServerEX, UaExpert) reject"
                .to_string(),
        });
    }

    let mut parsed = Vec::with_capacity(endpoints.len());
    let mut seen = HashSet::with_capacity(endpoints.len());
    for raw in endpoints {
        let endpoint = parse_advertised_endpoint(raw)?;
        let key = (
            endpoint.host.to_ascii_lowercase(),
            endpoint.port,
            endpoint.path.clone(),
        );
        if !seen.insert(key) {
            return Err(NorthwardError::ConfigurationError {
                message: format!("advertised_endpoints contains duplicate entry '{raw}'"),
            });
        }
        parsed.push(endpoint);
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(channel: &str, device: &str, key: &str) -> PointMeta {
        PointMeta {
            point_id: 1,
            channel_id: 1,
            channel_name: channel.into(),
            device_id: 1,
            device_name: device.into(),
            point_name: "name".into(),
            point_key: key.into(),
            data_type: DataType::Float64,
            point_type: DataPointType::Telemetry,
            access_mode: AccessMode::Read,
            unit: None,
            min_value: None,
            max_value: None,
            transform: Default::default(),
            description: None,
        }
    }

    #[test]
    fn data_type_mapping_is_total() {
        let cases = [
            (DataType::Boolean, "Boolean"),
            (DataType::Int8, "SByte"),
            (DataType::UInt8, "Byte"),
            (DataType::Int16, "Int16"),
            (DataType::UInt16, "UInt16"),
            (DataType::Int32, "Int32"),
            (DataType::UInt32, "UInt32"),
            (DataType::Int64, "Int64"),
            (DataType::UInt64, "UInt64"),
            (DataType::Float32, "Float"),
            (DataType::Float64, "Double"),
            (DataType::String, "String"),
            (DataType::Binary, "ByteString"),
            (DataType::Timestamp, "DateTime"),
        ];
        for (input, expected) in cases {
            assert_eq!(opcua_data_type_name(input), expected, "for {input:?}");
        }
    }

    #[test]
    fn access_level_label_includes_pipe_for_readwrite() {
        assert_eq!(opcua_access_level_label(AccessMode::Read), "CurrentRead");
        assert_eq!(opcua_access_level_label(AccessMode::Write), "CurrentWrite");
        assert_eq!(
            opcua_access_level_label(AccessMode::ReadWrite),
            "CurrentRead | CurrentWrite"
        );
    }

    #[test]
    fn point_type_and_access_mode_labels_use_snake_case() {
        assert_eq!(point_type_label(DataPointType::Telemetry), "telemetry");
        assert_eq!(point_type_label(DataPointType::Attribute), "attribute");
        assert_eq!(access_mode_label(AccessMode::ReadWrite), "read_write");
        assert_eq!(data_type_label(DataType::Float32), "float32");
        assert_eq!(data_type_label(DataType::Binary), "binary");
    }

    #[test]
    fn escape_segment_preserves_utf8_and_dot() {
        assert_eq!(escape_segment("通道一"), "通道一");
        assert_eq!(escape_segment("dev.1"), "dev.1");
        assert_eq!(escape_segment("ch 1"), "ch 1");
        assert_eq!(escape_segment("名称-A_b.c"), "名称-A_b.c");
    }

    #[test]
    fn escape_segment_replaces_separator_with_fullwidth() {
        assert_eq!(escape_segment("a/b/c"), "a／b／c");
    }

    #[test]
    fn escape_segment_collapses_control_characters() {
        assert_eq!(escape_segment("a\nb\tc\u{0001}d"), "a_b_c_d");
        assert_eq!(escape_segment("\u{007F}x"), "_x");
    }

    #[test]
    fn make_node_id_identifier_uses_slash_separator() {
        assert_eq!(
            make_node_id_identifier("ch", "dev", "key"),
            "ch/dev/key".to_string()
        );
    }

    #[test]
    fn make_node_id_preserves_chinese_segments() {
        let m = meta("通道一", "1号温湿度计", "湿度");
        let node_id = make_node_id(2, &m);
        assert_eq!(node_id.namespace, 2);
        assert_eq!(node_id.to_string(), "ns=2;s=通道一/1号温湿度计/湿度");
    }

    #[test]
    fn make_node_id_escapes_collision_slashes() {
        let m = meta("ch/1", "dev", "key");
        let node_id = make_node_id(2, &m);
        assert_eq!(node_id.to_string(), "ns=2;s=ch／1/dev/key");
    }

    #[test]
    fn browse_path_canonical_root() {
        assert_eq!(
            make_browse_path("ch", "dev", "key"),
            "/Objects/NG-Gateway/ch/dev/key"
        );
    }

    #[test]
    fn browse_path_preserves_chinese() {
        assert_eq!(
            make_browse_path("通道一", "1号温湿度计", "湿度"),
            "/Objects/NG-Gateway/通道一/1号温湿度计/湿度"
        );
    }

    #[test]
    fn browse_path_escapes_segment_slashes() {
        assert_eq!(
            make_browse_path("ch/1", "dev", "key"),
            "/Objects/NG-Gateway/ch／1/dev/key"
        );
    }

    #[test]
    fn parse_endpoint_accepts_hostname() {
        let e = parse_advertised_endpoint("opc.tcp://gateway.local:4840/").unwrap();
        assert_eq!(e.host, "gateway.local");
        assert_eq!(e.port, 4840);
        assert_eq!(e.path, "/");
    }

    #[test]
    fn parse_endpoint_accepts_ipv4() {
        let e = parse_advertised_endpoint("opc.tcp://192.168.1.10:4840").unwrap();
        assert_eq!(e.host, "192.168.1.10");
        assert_eq!(e.port, 4840);
        assert_eq!(e.path, "/"); // url crate normalizes empty -> "/"
    }

    #[test]
    fn parse_endpoint_accepts_ipv6_bracketed() {
        let e = parse_advertised_endpoint("opc.tcp://[fe80::1]:4840/path").unwrap();
        assert_eq!(e.host, "fe80::1");
        assert_eq!(e.port, 4840);
        assert_eq!(e.path, "/path");
    }

    #[test]
    fn parse_endpoint_uses_default_port_when_missing() {
        let e = parse_advertised_endpoint("opc.tcp://gateway.local/").unwrap();
        assert_eq!(e.port, DEFAULT_OPCUA_TCP_PORT);
    }

    #[test]
    fn parse_endpoint_rejects_wildcard_ipv4() {
        let err = parse_advertised_endpoint("opc.tcp://0.0.0.0:4840/").unwrap_err();
        let NorthwardError::ConfigurationError { message } = err else {
            panic!("expected ConfigurationError");
        };
        assert!(message.contains("0.0.0.0"));
    }

    #[test]
    fn parse_endpoint_rejects_wildcard_ipv6() {
        assert!(parse_advertised_endpoint("opc.tcp://[::]:4840/").is_err());
    }

    #[test]
    fn parse_endpoint_rejects_non_opc_scheme() {
        let err = parse_advertised_endpoint("https://gateway.local:4840/").unwrap_err();
        let NorthwardError::ConfigurationError { message } = err else {
            panic!("expected ConfigurationError");
        };
        assert!(message.contains("opc.tcp"));
    }

    #[test]
    fn parse_endpoint_rejects_empty() {
        assert!(parse_advertised_endpoint("").is_err());
        assert!(parse_advertised_endpoint("   ").is_err());
    }

    #[test]
    fn validate_endpoints_rejects_empty_list() {
        let err = validate_advertised_endpoints(&[]).unwrap_err();
        let NorthwardError::ConfigurationError { message } = err else {
            panic!("expected ConfigurationError");
        };
        assert!(message.contains("advertised_endpoints"));
    }

    #[test]
    fn validate_endpoints_rejects_duplicates() {
        let endpoints = vec![
            "opc.tcp://gateway.local:4840/".to_string(),
            "opc.tcp://gateway.local:4840/".to_string(),
        ];
        let err = validate_advertised_endpoints(&endpoints).unwrap_err();
        let NorthwardError::ConfigurationError { message } = err else {
            panic!("expected ConfigurationError");
        };
        assert!(message.to_lowercase().contains("duplicate"));
    }

    #[test]
    fn validate_endpoints_preserves_order_for_unique_entries() {
        let endpoints = vec![
            "opc.tcp://gateway.local:4840/".to_string(),
            "opc.tcp://192.168.1.10:4840/".to_string(),
        ];
        let parsed = validate_advertised_endpoints(&endpoints).unwrap();
        assert_eq!(parsed[0].host, "gateway.local");
        assert_eq!(parsed[1].host, "192.168.1.10");
    }

    #[test]
    fn endpoint_addr_canonical_brackets_ipv6() {
        let e = parse_advertised_endpoint("opc.tcp://[fe80::1]:4840/").unwrap();
        assert_eq!(e.canonical(), "opc.tcp://[fe80::1]:4840/");
    }
}
