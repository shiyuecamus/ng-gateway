use crate::envelope::EnvelopeKind;
use crate::mapping::{CompiledMappedJson, MappedJsonSpec, MappedRule};
use crate::northward::codec::{decode_downlink_envelope, DecodeError};
use crate::northward::payload::MappedJsonConfig;
use crate::{NorthwardEvent, ServerRpcResponse, WritePoint};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Downlink event kind used for mapping into `NorthwardEvent`.
#[derive(Debug, Clone, Copy)]
pub enum DownlinkKind {
    WritePoint,
    CommandReceived,
    RpcResponseReceived,
}

impl From<DownlinkKind> for EnvelopeKind {
    #[inline]
    fn from(value: DownlinkKind) -> Self {
        match value {
            DownlinkKind::WritePoint => EnvelopeKind::WritePoint,
            DownlinkKind::CommandReceived => EnvelopeKind::CommandReceived,
            DownlinkKind::RpcResponseReceived => EnvelopeKind::RpcResponseReceived,
        }
    }
}

impl TryFrom<EnvelopeKind> for DownlinkKind {
    type Error = ();

    #[inline]
    fn try_from(value: EnvelopeKind) -> Result<Self, Self::Error> {
        match value {
            EnvelopeKind::WritePoint => Ok(DownlinkKind::WritePoint),
            EnvelopeKind::CommandReceived => Ok(DownlinkKind::CommandReceived),
            EnvelopeKind::RpcResponseReceived => Ok(DownlinkKind::RpcResponseReceived),
            _ => Err(()),
        }
    }
}

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
    #[serde(default)]
    pub write_point: EventDownlink,
    #[serde(default)]
    pub command_received: EventDownlink,
    #[serde(default)]
    pub rpc_response_received: EventDownlink,
}

impl Default for DownlinkConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            write_point: EventDownlink::default(),
            command_received: EventDownlink::default(),
            rpc_response_received: EventDownlink::default(),
        }
    }
}

/// Default `true` helper for config toggles.
#[inline]
fn default_enabled_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDownlink {
    #[serde(default)]
    pub enabled: bool,
    /// Exact topic for subscription.
    ///
    /// Notes:
    /// - Downlink subscription only supports exact topics (NO templates / wildcards / regex).
    /// - Put request-scoped identifiers (e.g., request_id) in message key/properties/payload,
    ///   rather than encoding them into topic names.
    #[serde(default = "EventDownlink::default_downlink_topic")]
    pub topic: String,
    #[serde(default)]
    pub payload: DownlinkPayloadConfig,
    #[serde(default)]
    pub ack_policy: AckPolicy,
    #[serde(default)]
    pub failure_policy: FailurePolicy,
}

impl Default for EventDownlink {
    fn default() -> Self {
        Self {
            enabled: false,
            topic: EventDownlink::default_downlink_topic(),
            payload: DownlinkPayloadConfig::default(),
            ack_policy: AckPolicy::OnSuccess,
            failure_policy: FailurePolicy::Drop,
        }
    }
}

impl EventDownlink {
    fn default_downlink_topic() -> String {
        // Default to a single shared downlink topic. Users can split by event-kind if desired.
        "persistent://public/default/ng.downlink".to_string()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DownlinkPayloadConfig {
    /// Recommended stable envelope json:
    /// { schema_version, event:{kind}, payload:{data} }
    #[default]
    EnvelopeJson,
    /// Declarative mapping from input JSON to the target event struct.
    ///
    /// IMPORTANT:
    /// - When multiple event routes share one topic, `mapped_json` should set a filter
    ///   to avoid treating other event types as mapping errors.
    MappedJson {
        config: MappedJsonConfig,
        #[serde(default)]
        filter: MappedDownlinkFilterConfig,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MappedDownlinkFilterConfig {
    /// No filter. The route will always attempt to map JSON into the target event struct.
    ///
    /// NOTE: If multiple routes share the same topic, leaving this as `none` may cause
    /// noisy mapping errors for messages that belong to other routes.
    #[default]
    None,
    /// Extract a discriminator from JSON using JSON Pointer and compare with the expected value.
    ///
    /// Example pointers:
    /// - `/event_type`
    /// - `/event/kind`
    JsonPointer {
        /// JSON Pointer string (RFC 6901).
        pointer: String,
        /// Expected value (string comparison).
        equals: String,
    },
    /// Compare a message property (key/value) with the expected value.
    /// (Replaced `PulsarProperty` to be generic)
    Property {
        /// Property key.
        key: String,
        /// Expected property value.
        equals: String,
    },
    /// Compare message key with the expected value.
    /// (Replaced `PulsarKey` to be generic)
    Key {
        /// Expected message key.
        equals: String,
    },
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AckPolicy {
    /// Ack only when successfully decoded and forwarded.
    #[default]
    OnSuccess,
    /// Always ack even on decode errors (drop-on-failure semantics).
    Always,
    /// Never ack (debug / external ack management).
    Never,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    #[default]
    Drop,
    Error,
}

/// One downlink route (event slot) built from `EventDownlink` config.
///
/// Notes:
/// - Topic matching is NOT handled here. Downlink only supports exact topics.
/// - Multiple routes can share the same topic.
#[derive(Debug, Clone)]
pub struct DownlinkRoute {
    pub kind: DownlinkKind,
    pub mapping: EventDownlink,
}

/// Route table compiled from enabled downlink routes.
///
/// Design goals:
/// - Exact topic subscription only.
/// - Allow multiple routes on one topic (mixed event types).
/// - Pre-group routes by topic for fast lookup in the consumer loop.
#[derive(Debug)]
pub struct DownlinkRouteTable {
    pub topics: Arc<[String]>,
    pub by_topic: HashMap<String, Arc<[DownlinkRoute]>>,
}

/// Generic key-value pair for message properties/headers
#[derive(Debug, Clone, Copy)]
pub struct KeyValue<'a> {
    pub key: &'a str,
    pub value: &'a str,
}

/// Downlink message metadata passed to filters.
#[derive(Debug, Clone, Copy)]
pub struct DownlinkMessageMeta<'a> {
    /// Message key (partition_key), if available.
    pub key: Option<&'a str>,
    /// Message properties, if available.
    pub properties: Option<&'a [KeyValue<'a>]>,
}

/// Validate and normalize an exact Pulsar/Kafka topic string.
///
/// We intentionally reject templates/wildcards/regex-like prefixes to keep operational
/// behavior predictable.
pub fn validate_exact_topic(topic: &str) -> Result<String, String> {
    let t = topic.trim();
    if t.is_empty() {
        return Err("downlink topic is empty".to_string());
    }
    if t.contains("{{") || t.contains("}}") {
        return Err("downlink topic must be exact (templates are not supported)".to_string());
    }
    if t.contains('*') {
        return Err("downlink topic must be exact (wildcards are not supported)".to_string());
    }
    if t.starts_with("re:") || t.starts_with("regex:") {
        return Err("downlink topic must be exact (regex is not supported)".to_string());
    }
    Ok(t.to_string())
}

/// Build a route table from enabled routes.
pub fn build_route_table(routes: Vec<DownlinkRoute>) -> Result<DownlinkRouteTable, String> {
    let mut by_topic: HashMap<String, Vec<DownlinkRoute>> = HashMap::new();

    for mut r in routes {
        let normalized = validate_exact_topic(&r.mapping.topic)?;
        // Normalize to trimmed topic to keep grouping stable.
        r.mapping.topic = normalized.clone();
        by_topic.entry(normalized).or_default().push(r);
    }

    // Validate per-topic policies to avoid ambiguous ack/nack behavior.
    //
    // When multiple routes share one topic, they must agree on ack/failure policy.
    for (topic, rs) in by_topic.iter() {
        let Some(first) = rs.first() else {
            continue;
        };
        for r in rs.iter().skip(1) {
            if r.mapping.ack_policy != first.mapping.ack_policy {
                return Err(format!(
                    "downlink routes on the same topic must share the same ack_policy (topic={topic})"
                ));
            }
            if r.mapping.failure_policy != first.mapping.failure_policy {
                return Err(format!(
                    "downlink routes on the same topic must share the same failure_policy (topic={topic})"
                ));
            }
        }
    }

    // Stable topic list (unique).
    let mut topics: Vec<String> = by_topic.keys().cloned().collect();
    topics.sort();

    // Compile per-topic route arrays (sorted by specificity to reduce work/noise).
    let mut compiled: HashMap<String, Arc<[DownlinkRoute]>> =
        HashMap::with_capacity(by_topic.len());
    for (topic, mut rs) in by_topic {
        rs.sort_by_key(|r| -route_specificity(r));
        compiled.insert(topic, Arc::from(rs.into_boxed_slice()));
    }

    Ok(DownlinkRouteTable {
        topics: Arc::from(topics.into_boxed_slice()),
        by_topic: compiled,
    })
}

#[inline]
fn route_specificity(r: &DownlinkRoute) -> i32 {
    match &r.mapping.payload {
        // EnvelopeJson can cheaply ignore mismatched event kinds by reading event.kind.
        DownlinkPayloadConfig::EnvelopeJson => 50,
        DownlinkPayloadConfig::MappedJson { filter, .. } => match filter {
            MappedDownlinkFilterConfig::None => 0,
            // Prefer metadata-based filters, then JSON-pointer filters.
            MappedDownlinkFilterConfig::Property { .. }
            | MappedDownlinkFilterConfig::Key { .. } => 100,
            MappedDownlinkFilterConfig::JsonPointer { .. } => 80,
        },
    }
}

/// Evaluate whether the route filter matches the current message.
fn mapped_filter_matches(
    meta: &DownlinkMessageMeta<'_>,
    input: &Value,
    filter: &MappedDownlinkFilterConfig,
) -> bool {
    match filter {
        MappedDownlinkFilterConfig::None => true,
        MappedDownlinkFilterConfig::JsonPointer { pointer, equals } => {
            let Some(x) = input.pointer(pointer.as_str()) else {
                return false;
            };
            match x {
                Value::String(s) => s == equals,
                Value::Number(n) => n.to_string() == *equals,
                Value::Bool(b) => b.to_string() == *equals,
                _ => false,
            }
        }
        MappedDownlinkFilterConfig::Property { key, equals } => {
            let Some(props) = meta.properties else {
                return false;
            };
            props.iter().any(|kv| kv.key == *key && kv.value == *equals)
        }
        MappedDownlinkFilterConfig::Key { equals } => meta.key.is_some_and(|k| k == equals),
    }
}

/// Decode a downlink message into a northward event.
pub fn decode_event(
    route: &DownlinkRoute,
    meta: &DownlinkMessageMeta<'_>,
    bytes: &[u8],
) -> Result<Option<NorthwardEvent>, DecodeError> {
    match &route.mapping.payload {
        DownlinkPayloadConfig::EnvelopeJson => {
            decode_downlink_envelope(bytes, EnvelopeKind::from(route.kind))
        }
        DownlinkPayloadConfig::MappedJson { config, filter } => {
            decode_mapped(route.kind, meta, bytes, config, filter)
        }
    }
}

fn decode_mapped(
    kind: DownlinkKind,
    meta: &DownlinkMessageMeta<'_>,
    bytes: &[u8],
    cfg: &MappedJsonConfig,
    filter: &MappedDownlinkFilterConfig,
) -> Result<Option<NorthwardEvent>, DecodeError> {
    let input: Value = serde_json::from_slice(bytes)?;

    // IMPORTANT: `mapped_json` filter is part of decode.
    // If not matched, return Ok(None) (ignored, not an error).
    if !mapped_filter_matches(meta, &input, filter) {
        return Ok(None);
    }

    let rules: Vec<MappedRule> = cfg
        .iter()
        .map(|(out_path, expr)| MappedRule {
            out_path: out_path.clone(),
            expr: expr.clone(),
        })
        .collect();
    let spec = MappedJsonSpec { rules };
    let compiled = CompiledMappedJson::compile(&spec)?;

    let out = compiled.apply(&input)?;

    match kind {
        DownlinkKind::WritePoint => {
            let wp: WritePoint =
                serde_json::from_value(out).map_err(|e| DecodeError::Payload(e.to_string()))?;
            Ok(Some(NorthwardEvent::WritePoint(wp)))
        }
        DownlinkKind::CommandReceived => {
            let cmd: crate::Command =
                serde_json::from_value(out).map_err(|e| DecodeError::Payload(e.to_string()))?;
            Ok(Some(NorthwardEvent::CommandReceived(cmd)))
        }
        DownlinkKind::RpcResponseReceived => {
            let resp: ServerRpcResponse =
                serde_json::from_value(out).map_err(|e| DecodeError::Payload(e.to_string()))?;
            Ok(Some(NorthwardEvent::RpcResponseReceived(resp)))
        }
    }
}
