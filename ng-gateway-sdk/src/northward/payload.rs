use crate::{
    mapping::{build_mapping_input, CompiledMappedJson, MappedJsonSpec, MappedRule},
    northward::{
        codec::{encode_uplink, EncodeError, NorthwardUplinkFormat},
        NorthwardData, NorthwardRuntimeApi,
    },
};
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{ser::SerializeMap, Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

/// Declarative mapping configuration for `mapped_json`.
///
/// JSON shape:
/// - `{ "out.path": "<jmespath expr>", ... }`
pub type MappedJsonConfig = BTreeMap<String, String>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum UplinkPayloadConfig {
    /// Serialize a stable envelope JSON (recommended default).
    #[default]
    EnvelopeJson,
    /// Telemetry/attributes optimized {ts, values:{k:v}}.
    Kv {
        #[serde(rename = "includeMeta", default)]
        include_meta: bool,
    },
    /// Rows array friendly for lakes / TSDB ingestion.
    TimeseriesRows {
        #[serde(rename = "includeMeta", default)]
        include_meta: bool,
    },
    /// Declarative mapping: output a custom JSON object built from expressions.
    MappedJson { config: MappedJsonConfig },
}

#[derive(Debug, Clone, Copy)]
pub enum UplinkEventKind {
    DeviceConnected,
    DeviceDisconnected,
    Telemetry,
    Attributes,
}

impl UplinkEventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            UplinkEventKind::DeviceConnected => "device_connected",
            UplinkEventKind::DeviceDisconnected => "device_disconnected",
            UplinkEventKind::Telemetry => "telemetry",
            UplinkEventKind::Attributes => "attributes",
        }
    }
}

impl Serialize for UplinkEventKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for UplinkEventKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "device_connected" => Ok(UplinkEventKind::DeviceConnected),
            "device_disconnected" => Ok(UplinkEventKind::DeviceDisconnected),
            "telemetry" => Ok(UplinkEventKind::Telemetry),
            "attributes" => Ok(UplinkEventKind::Attributes),
            other => Err(serde::de::Error::custom(format!(
                "unknown event_kind: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderContext {
    pub app_id: i32,
    pub app_name: String,
    pub plugin_type: String,
    pub event_kind: UplinkEventKind,
    pub ts: DateTime<Utc>,
    pub device_id: i32,
    pub device_name: String,
    pub device_type: Option<String>,
    pub channel_name: Option<String>,
}

/// Borrowed render context for hot-path uplink processing.
///
/// This avoids per-message cloning of `String` fields (app/device names) when the caller
/// already owns them (e.g. plugin state or the incoming `NorthwardData`).
///
/// # Notes
/// - This type is intended for high-throughput paths (topic/key templates and payload encoding).
/// - It is safe to serialize; any required owned data (e.g. `channel_name`) is held by `Arc<str>`.
#[derive(Debug, Clone)]
pub struct RenderContextRef<'a> {
    pub app_id: i32,
    pub app_name: Arc<str>,
    pub plugin_type: Arc<str>,
    pub event_kind: UplinkEventKind,
    pub ts: DateTime<Utc>,
    pub device_id: i32,
    pub device_name: &'a str,
    pub device_type: Option<&'a str>,
    pub channel_name: Option<Arc<str>>,
}

impl RenderContextRef<'_> {
    /// Convert the context into string properties for headers / message properties.
    ///
    /// # Notes
    /// This allocates `String` values because downstream transports typically require owned
    /// strings. Prefer using this only when the platform actually uses headers/properties.
    pub fn to_properties_map(&self) -> HashMap<String, String> {
        // Pre-allocate to reduce per-message allocations on hot paths.
        let mut props = HashMap::with_capacity(8);
        props.insert("app_id".to_string(), self.app_id.to_string());
        props.insert("app_name".to_string(), self.app_name.as_ref().to_string());
        props.insert(
            "plugin_type".to_string(),
            self.plugin_type.as_ref().to_string(),
        );
        props.insert(
            "event_kind".to_string(),
            self.event_kind.as_str().to_string(),
        );
        props.insert("ts_ms".to_string(), self.ts.timestamp_millis().to_string());
        props.insert("device_id".to_string(), self.device_id.to_string());
        props.insert("device_name".to_string(), self.device_name.to_string());
        if let Some(dt) = self.device_type {
            props.insert("device_type".to_string(), dt.to_string());
        }
        if let Some(ch) = &self.channel_name {
            props.insert("channel_name".to_string(), ch.as_ref().to_string());
        }
        props
    }
}

impl From<RenderContext> for HashMap<String, String> {
    fn from(ctx: RenderContext) -> Self {
        // Pre-allocate to reduce per-message allocations on hot paths.
        let mut props = HashMap::with_capacity(8);
        props.insert("app_id".to_string(), ctx.app_id.to_string());
        props.insert("app_name".to_string(), ctx.app_name);
        props.insert("plugin_type".to_string(), ctx.plugin_type);
        props.insert(
            "event_kind".to_string(),
            ctx.event_kind.as_str().to_string(),
        );
        props.insert("ts_ms".to_string(), ctx.ts.timestamp_millis().to_string());
        props.insert("device_id".to_string(), ctx.device_id.to_string());
        props.insert("device_name".to_string(), ctx.device_name);
        if let Some(dt) = ctx.device_type {
            props.insert("device_type".to_string(), dt);
        }
        if let Some(ch) = ctx.channel_name {
            props.insert("channel_name".to_string(), ch);
        }
        props
    }
}

/// Serialize RenderContext as a flat object of template variables, all string values.
///
/// This is intentionally aligned with the previous `to_kv()` semantics so templates stay stable.
impl Serialize for RenderContext {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("app_id", &self.app_id.to_string())?;
        map.serialize_entry("app_name", &self.app_name)?;
        map.serialize_entry("plugin_type", &self.plugin_type)?;
        map.serialize_entry("event_kind", self.event_kind.as_str())?;
        map.serialize_entry("ts_ms", &self.ts.timestamp_millis().to_string())?;
        map.serialize_entry("device_id", &self.device_id.to_string())?;
        map.serialize_entry("device_name", &self.device_name)?;
        if let Some(dt) = &self.device_type {
            map.serialize_entry("device_type", dt)?;
        }
        if let Some(ch) = &self.channel_name {
            map.serialize_entry("channel_name", ch)?;
        }
        // time partitions (UTC)
        map.serialize_entry("yyyy", &format!("{:04}", self.ts.year()))?;
        map.serialize_entry("MM", &format!("{:02}", self.ts.month()))?;
        map.serialize_entry("dd", &format!("{:02}", self.ts.day()))?;
        map.serialize_entry("HH", &format!("{:02}", self.ts.hour()))?;
        map.end()
    }
}

/// Serialize `RenderContextRef` as a flat object of template variables, all string values.
///
/// This is intentionally aligned with `RenderContext` so templates stay stable.
impl Serialize for RenderContextRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("app_id", &self.app_id.to_string())?;
        map.serialize_entry("app_name", self.app_name.as_ref())?;
        map.serialize_entry("plugin_type", self.plugin_type.as_ref())?;
        map.serialize_entry("event_kind", self.event_kind.as_str())?;
        map.serialize_entry("ts_ms", &self.ts.timestamp_millis().to_string())?;
        map.serialize_entry("device_id", &self.device_id.to_string())?;
        map.serialize_entry("device_name", self.device_name)?;
        if let Some(dt) = self.device_type {
            map.serialize_entry("device_type", dt)?;
        }
        if let Some(ch) = &self.channel_name {
            map.serialize_entry("channel_name", ch.as_ref())?;
        }
        // time partitions (UTC)
        map.serialize_entry("yyyy", &format!("{:04}", self.ts.year()))?;
        map.serialize_entry("MM", &format!("{:02}", self.ts.month()))?;
        map.serialize_entry("dd", &format!("{:02}", self.ts.day()))?;
        map.serialize_entry("HH", &format!("{:02}", self.ts.hour()))?;
        map.end()
    }
}

/// Build a render context from northward data.
///
/// Note: Telemetry/Attributes do not carry `device_type`. We keep it `None` for now.
pub fn build_context(
    app_id: i32,
    app_name: &str,
    plugin_type: &str,
    event_kind: UplinkEventKind,
    data: &NorthwardData,
    runtime: &Arc<dyn NorthwardRuntimeApi>,
) -> Option<RenderContext> {
    match data {
        NorthwardData::DeviceConnected(d) => Some(RenderContext {
            app_id,
            app_name: app_name.to_string(),
            plugin_type: plugin_type.to_string(),
            event_kind,
            ts: Utc::now(),
            device_id: d.device_id,
            device_name: d.device_name.clone(),
            device_type: Some(d.device_type.clone()),
            channel_name: None,
        }),
        NorthwardData::DeviceDisconnected(d) => Some(RenderContext {
            app_id,
            app_name: app_name.to_string(),
            plugin_type: plugin_type.to_string(),
            event_kind,
            ts: Utc::now(),
            device_id: d.device_id,
            device_name: d.device_name.clone(),
            device_type: Some(d.device_type.clone()),
            channel_name: None,
        }),
        NorthwardData::Telemetry(t) => {
            let ch = t
                .values
                .first()
                .and_then(|pv| runtime.get_point_meta(pv.point_id))
                .map(|m| m.channel_name.as_ref().to_string());
            Some(RenderContext {
                app_id,
                app_name: app_name.to_string(),
                plugin_type: plugin_type.to_string(),
                event_kind,
                ts: t.timestamp,
                device_id: t.device_id,
                device_name: t.device_name.clone(),
                device_type: None,
                channel_name: ch,
            })
        }
        NorthwardData::Attributes(a) => {
            let first = a
                .client_attributes
                .first()
                .or(a.shared_attributes.first())
                .or(a.server_attributes.first());
            let ch = first
                .and_then(|pv| runtime.get_point_meta(pv.point_id))
                .map(|m| m.channel_name.as_ref().to_string());
            Some(RenderContext {
                app_id,
                app_name: app_name.to_string(),
                plugin_type: plugin_type.to_string(),
                event_kind,
                ts: a.timestamp,
                device_id: a.device_id,
                device_name: a.device_name.clone(),
                device_type: None,
                channel_name: ch,
            })
        }
        _ => None,
    }
}

/// Build a borrowed render context from northward data.
///
/// # Notes
/// - Telemetry/Attributes do not carry `device_type`. We keep it `None` for now.
/// - `channel_name` is captured as `Arc<str>` to keep the reference alive without cloning strings.
pub fn build_context_ref<'a>(
    app_id: i32,
    app_name: &Arc<str>,
    plugin_type: &Arc<str>,
    event_kind: UplinkEventKind,
    data: &'a NorthwardData,
    runtime: &Arc<dyn NorthwardRuntimeApi>,
) -> Option<RenderContextRef<'a>> {
    match data {
        NorthwardData::DeviceConnected(d) => Some(RenderContextRef {
            app_id,
            app_name: Arc::clone(app_name),
            plugin_type: Arc::clone(plugin_type),
            event_kind,
            ts: Utc::now(),
            device_id: d.device_id,
            device_name: d.device_name.as_str(),
            device_type: Some(d.device_type.as_str()),
            channel_name: None,
        }),
        NorthwardData::DeviceDisconnected(d) => Some(RenderContextRef {
            app_id,
            app_name: Arc::clone(app_name),
            plugin_type: Arc::clone(plugin_type),
            event_kind,
            ts: Utc::now(),
            device_id: d.device_id,
            device_name: d.device_name.as_str(),
            device_type: Some(d.device_type.as_str()),
            channel_name: None,
        }),
        NorthwardData::Telemetry(t) => {
            let ch = t
                .values
                .first()
                .and_then(|pv| runtime.get_point_meta(pv.point_id))
                .map(|m| Arc::clone(&m.channel_name));
            Some(RenderContextRef {
                app_id,
                app_name: Arc::clone(app_name),
                plugin_type: Arc::clone(plugin_type),
                event_kind,
                ts: t.timestamp,
                device_id: t.device_id,
                device_name: t.device_name.as_str(),
                device_type: None,
                channel_name: ch,
            })
        }
        NorthwardData::Attributes(a) => {
            let first = a
                .client_attributes
                .first()
                .or(a.shared_attributes.first())
                .or(a.server_attributes.first());
            let ch = first
                .and_then(|pv| runtime.get_point_meta(pv.point_id))
                .map(|m| Arc::clone(&m.channel_name));
            Some(RenderContextRef {
                app_id,
                app_name: Arc::clone(app_name),
                plugin_type: Arc::clone(plugin_type),
                event_kind,
                ts: a.timestamp,
                device_id: a.device_id,
                device_name: a.device_name.as_str(),
                device_type: None,
                channel_name: ch,
            })
        }
        _ => None,
    }
}

pub fn encode_uplink_payload(
    mode: &UplinkPayloadConfig,
    ctx: &RenderContext,
    data: &NorthwardData,
    runtime: &Arc<dyn NorthwardRuntimeApi>,
) -> Result<Vec<u8>, EncodeError> {
    // NOTE: this is a compatibility path; it will allocate `Arc<str>` wrappers for meta fields.
    let plugin_type: Arc<str> = Arc::from(ctx.plugin_type.as_str());
    let app_name: Arc<str> = Arc::from(ctx.app_name.as_str());
    match mode {
        UplinkPayloadConfig::EnvelopeJson => encode_uplink(
            NorthwardUplinkFormat::EnvelopeJson,
            &plugin_type,
            ctx.app_id,
            &app_name,
            data,
            runtime.as_ref(),
        ),
        UplinkPayloadConfig::Kv { include_meta } => encode_uplink(
            NorthwardUplinkFormat::Kv {
                include_meta: *include_meta,
            },
            &plugin_type,
            ctx.app_id,
            &app_name,
            data,
            runtime.as_ref(),
        ),
        UplinkPayloadConfig::TimeseriesRows { include_meta } => encode_uplink(
            NorthwardUplinkFormat::TimeseriesRows {
                include_meta: *include_meta,
            },
            &plugin_type,
            ctx.app_id,
            &app_name,
            data,
            runtime.as_ref(),
        ),
        UplinkPayloadConfig::MappedJson { config } => {
            let v = encode_mapped_json(ctx, data, config)?;
            serde_json::to_vec(&v).map_err(EncodeError::Json)
        }
    }
}

/// Encode uplink payload using a borrowed render context.
///
/// This is a thin wrapper over `encode_uplink_payload` that avoids requiring an owned
/// `RenderContext` on hot paths.
pub fn encode_uplink_payload_ref(
    mode: &UplinkPayloadConfig,
    ctx: &RenderContextRef<'_>,
    data: &NorthwardData,
    runtime: &Arc<dyn NorthwardRuntimeApi>,
) -> Result<Vec<u8>, EncodeError> {
    match mode {
        UplinkPayloadConfig::EnvelopeJson => encode_uplink(
            NorthwardUplinkFormat::EnvelopeJson,
            &ctx.plugin_type,
            ctx.app_id,
            &ctx.app_name,
            data,
            runtime.as_ref(),
        ),
        UplinkPayloadConfig::Kv { include_meta } => encode_uplink(
            NorthwardUplinkFormat::Kv {
                include_meta: *include_meta,
            },
            &ctx.plugin_type,
            ctx.app_id,
            &ctx.app_name,
            data,
            runtime.as_ref(),
        ),
        UplinkPayloadConfig::TimeseriesRows { include_meta } => encode_uplink(
            NorthwardUplinkFormat::TimeseriesRows {
                include_meta: *include_meta,
            },
            &ctx.plugin_type,
            ctx.app_id,
            &ctx.app_name,
            data,
            runtime.as_ref(),
        ),
        UplinkPayloadConfig::MappedJson { config } => {
            let v = encode_mapped_json_ref(ctx, data, config)?;
            serde_json::to_vec(&v).map_err(EncodeError::Json)
        }
    }
}

fn encode_mapped_json(
    ctx: &RenderContext,
    data: &NorthwardData,
    cfg: &MappedJsonConfig,
) -> Result<Value, EncodeError> {
    let rules: Vec<MappedRule> = cfg
        .iter()
        .map(|(out_path, expr)| MappedRule {
            out_path: out_path.clone(),
            expr: expr.clone(),
        })
        .collect();
    let spec = MappedJsonSpec { rules };
    let compiled = CompiledMappedJson::compile(&spec)?;

    // Canonical mapping input view is provided by SDK.
    let input = build_mapping_input(
        ctx.plugin_type.as_str(),
        ctx.app_id,
        ctx.app_name.as_str(),
        data,
    );
    Ok(compiled.apply(&input)?)
}

fn encode_mapped_json_ref(
    ctx: &RenderContextRef<'_>,
    data: &NorthwardData,
    cfg: &MappedJsonConfig,
) -> Result<Value, EncodeError> {
    let rules: Vec<MappedRule> = cfg
        .iter()
        .map(|(out_path, expr)| MappedRule {
            out_path: out_path.clone(),
            expr: expr.clone(),
        })
        .collect();
    let spec = MappedJsonSpec { rules };
    let compiled = CompiledMappedJson::compile(&spec)?;

    let input = build_mapping_input(
        ctx.plugin_type.as_ref(),
        ctx.app_id,
        ctx.app_name.as_ref(),
        data,
    );
    Ok(compiled.apply(&input)?)
}
