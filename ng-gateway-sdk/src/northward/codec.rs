//! Northward codec utilities shared by all northward plugins.
//!
//! This module intentionally lives in the SDK so multiple plugins (MQTT/HTTP/Kafka/Pulsar...)
//! can share **one** canonical definition of:
//! - Envelope kind mapping (`NorthwardData`/`NorthwardEvent` <-> `EnvelopeKind`)
//! - Uplink encoding formats (`envelope_json`, `kv`, `timeseries_rows`)
//! - Downlink decoding for `envelope_json`
//!
//! IMPORTANT:
//! - Wire format must remain stable. Do NOT introduce additional tags inside `payload.data`.
//! - Routing/selection is performed by `event.kind`.

use crate::{
    envelope::{
        EnvelopeApp, EnvelopeDevice, EnvelopeKind, EnvelopeMeta, NorthwardEnvelope,
        NorthwardEnvelopePayload, NorthwardEnvelopePayloadRef, NorthwardEnvelopeRef, WireEnvelope,
    },
    mapping::MappingError,
    northward::{runtime_api::NorthwardRuntimeApi, NorthwardData, NorthwardEvent},
    DataType, NGValue, PointValue,
};
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use thiserror::Error;

/// Uplink encoding formats supported by SDK.
#[derive(Debug, Clone, Copy)]
pub enum NorthwardUplinkFormat {
    /// Stable envelope json:
    /// `{ schema_version, event:{kind}, envelope:{...}, payload:{data} }`
    EnvelopeJson,
    /// Telemetry/attributes optimized `{ts_ms, values:{k:v}}`.
    Kv {
        /// Whether to include lightweight type info inside `values.{key}` as
        /// `{ value, data_type }`.
        include_meta: bool,
    },
    /// Rows array friendly for lakes / TSDB ingestion.
    TimeseriesRows {
        /// Whether to include lightweight meta fields in each row.
        include_meta: bool,
    },
}

/// Uplink encoding errors.
#[derive(Debug, Error)]
pub enum EncodeError {
    /// JSON serialization failed.
    #[error("json encode failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Declarative mapping failed (mapped_json).
    #[error("mapping failed: {0}")]
    Mapping(#[from] MappingError),
}

/// Downlink decoding errors.
#[derive(Debug, Error)]
pub enum DecodeError {
    /// JSON parse failed.
    #[error("json decode failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Schema validation failed (e.g. schema_version mismatch).
    #[error("schema error: {0}")]
    Schema(String),
    /// Payload decode failed for matched kind.
    #[error("payload decode failed: {0}")]
    Payload(String),
    /// Declarative mapping failed (mapped_json).
    #[error("mapping failed: {0}")]
    Mapping(#[from] MappingError),
}

/// Encode a `NorthwardData` into bytes for a specific uplink format.
///
/// # Notes
/// - This function is designed for **high-throughput** paths: it avoids allocations where possible
///   and uses integer timestamps (unix ms) to reduce CPU overhead.
/// - For unsupported combinations (e.g., `Kv` on `DeviceConnected`), this returns a stable
///   fallback JSON object rather than an error, to keep configs tolerant.
pub fn encode_uplink(
    format: NorthwardUplinkFormat,
    plugin_type: &str,
    app_id: i32,
    app_name: &str,
    data: &NorthwardData,
    runtime: &dyn NorthwardRuntimeApi,
) -> Result<Vec<u8>, EncodeError> {
    match format {
        NorthwardUplinkFormat::EnvelopeJson => {
            let env = NorthwardEnvelopeRef::v1(NorthwardEnvelopePayloadRef::Data(data))
                .with_meta(build_envelope_meta(plugin_type, app_id, app_name, data));
            Ok(serde_json::to_vec(&env)?)
        }
        NorthwardUplinkFormat::Kv { include_meta } => {
            let kv = build_kv(data, runtime, include_meta);
            Ok(serde_json::to_vec(&kv)?)
        }
        NorthwardUplinkFormat::TimeseriesRows { include_meta } => {
            let rows = build_timeseries_rows(data, runtime, include_meta);
            Ok(serde_json::to_vec(&rows)?)
        }
    }
}

/// Decode downlink `envelope_json` bytes into `NorthwardEvent`.
///
/// Return semantics:
/// - Ok(Some(ev)) => successfully decoded and matched the expected kind.
/// - Ok(None)     => message is valid but does not belong to this route (ignored, NOT an error).
/// - Err(e)       => message claimed to be this kind but failed to decode (error).
pub fn decode_downlink_envelope(
    bytes: &[u8],
    expected_kind: EnvelopeKind,
) -> Result<Option<NorthwardEvent>, DecodeError> {
    let wire: WireEnvelope<Value> = serde_json::from_slice(bytes)?;
    if wire.schema_version != 1 {
        return Err(DecodeError::Schema(format!(
            "unsupported schema_version {}",
            wire.schema_version
        )));
    }
    if wire.event.kind != expected_kind {
        return Ok(None);
    }

    // Now that kind is matched, decode the strongly-typed payload and validate shape.
    let env = NorthwardEnvelope::try_from(wire).map_err(|e| DecodeError::Payload(e.to_string()))?;

    match (expected_kind, env.payload) {
        (
            EnvelopeKind::WritePoint,
            NorthwardEnvelopePayload::Event(ev @ NorthwardEvent::WritePoint(_)),
        ) => Ok(Some(ev)),
        (
            EnvelopeKind::CommandReceived,
            NorthwardEnvelopePayload::Event(ev @ NorthwardEvent::CommandReceived(_)),
        ) => Ok(Some(ev)),
        (
            EnvelopeKind::RpcResponseReceived,
            NorthwardEnvelopePayload::Event(ev @ NorthwardEvent::RpcResponseReceived(_)),
        ) => Ok(Some(ev)),
        (other, _) => Err(DecodeError::Schema(format!(
            "expected_kind {other:?} is not a supported downlink kind"
        ))),
    }
}

fn build_envelope_meta(
    plugin_type: &str,
    app_id: i32,
    app_name: &str,
    data: &NorthwardData,
) -> EnvelopeMeta {
    let (device_id, device_name, device_type) = match data {
        NorthwardData::DeviceConnected(d) => (
            d.device_id,
            d.device_name.clone(),
            Some(d.device_type.clone()),
        ),
        NorthwardData::DeviceDisconnected(d) => (
            d.device_id,
            d.device_name.clone(),
            Some(d.device_type.clone()),
        ),
        NorthwardData::Telemetry(t) => (t.device_id, t.device_name.clone(), None),
        NorthwardData::Attributes(a) => (a.device_id, a.device_name.clone(), None),
        NorthwardData::Alarm(a) => (a.device_id, a.device_name.clone(), None),
        NorthwardData::RpcResponse(r) => {
            (r.device_id, r.device_name.clone().unwrap_or_default(), None)
        }
        NorthwardData::WritePointResponse(r) => (r.device_id, String::new(), None),
    };

    let ts_ms = match data {
        NorthwardData::DeviceConnected(_) | NorthwardData::DeviceDisconnected(_) => {
            Utc::now().timestamp_millis()
        }
        NorthwardData::Telemetry(t) => t.timestamp.timestamp_millis(),
        NorthwardData::Attributes(a) => a.timestamp.timestamp_millis(),
        NorthwardData::Alarm(a) => a.timestamp.timestamp_millis(),
        NorthwardData::RpcResponse(r) => r.timestamp.timestamp_millis(),
        NorthwardData::WritePointResponse(r) => r.completed_at.timestamp_millis(),
    };

    EnvelopeMeta {
        ts_ms,
        app: EnvelopeApp {
            id: app_id,
            name: app_name.to_string(),
            plugin_type: plugin_type.to_string(),
        },
        device: EnvelopeDevice {
            id: device_id,
            name: device_name,
            r#type: device_type,
        },
    }
}

#[derive(Debug, Serialize)]
pub struct KvEnvelope {
    ts_ms: i64,
    values: BTreeMap<Arc<str>, KvValue>,
}

#[derive(Debug, Serialize)]
pub struct KvTypedValue {
    value: NGValue,
    data_type: DataType,
}

/// KV item value:
/// - `include_meta=false` => plain scalar (bool/number/string/base64)
/// - `include_meta=true`  => `{ value, data_type }`
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum KvValue {
    Plain(NGValue),
    Typed(KvTypedValue),
}

fn build_kv(
    data: &NorthwardData,
    runtime: &dyn NorthwardRuntimeApi,
    include_meta: bool,
) -> KvEnvelope {
    match data {
        NorthwardData::Telemetry(t) => {
            let mut values: BTreeMap<Arc<str>, KvValue> = BTreeMap::new();
            for pv in t.values.iter() {
                if include_meta {
                    if let Some(pm) = runtime.get_point_meta(pv.point_id) {
                        values.insert(
                            Arc::clone(&pv.point_key),
                            KvValue::Typed(KvTypedValue {
                                value: pv.value.clone(),
                                data_type: pm.data_type,
                            }),
                        );
                        continue;
                    }
                }
                values.insert(Arc::clone(&pv.point_key), KvValue::Plain(pv.value.clone()));
            }
            KvEnvelope {
                ts_ms: t.timestamp.timestamp_millis(),
                values,
            }
        }
        NorthwardData::Attributes(a) => {
            let mut values: BTreeMap<Arc<str>, KvValue> = BTreeMap::new();
            for pv in a
                .client_attributes
                .iter()
                .chain(a.shared_attributes.iter())
                .chain(a.server_attributes.iter())
            {
                if include_meta {
                    if let Some(pm) = runtime.get_point_meta(pv.point_id) {
                        values.insert(
                            Arc::clone(&pv.point_key),
                            KvValue::Typed(KvTypedValue {
                                value: pv.value.clone(),
                                data_type: pm.data_type,
                            }),
                        );
                        continue;
                    }
                }
                values.insert(Arc::clone(&pv.point_key), KvValue::Plain(pv.value.clone()));
            }
            KvEnvelope {
                ts_ms: a.timestamp.timestamp_millis(),
                values,
            }
        }
        _ => unreachable!(),
    }
}

#[derive(Debug, Serialize)]
pub struct TimeseriesRow {
    ts_ms: i64,
    point_id: i32,
    point_key: Arc<str>,
    value: NGValue,
    data_type: Option<DataType>,
}

fn build_timeseries_rows(
    data: &NorthwardData,
    runtime: &dyn NorthwardRuntimeApi,
    include_meta: bool,
) -> Vec<TimeseriesRow> {
    let mk_row = |ts_ms: i64, pv: &PointValue| -> TimeseriesRow {
        let data_type = if include_meta {
            runtime.get_point_meta(pv.point_id).map(|m| m.data_type)
        } else {
            None
        };
        TimeseriesRow {
            ts_ms,
            point_id: pv.point_id,
            point_key: Arc::clone(&pv.point_key),
            value: pv.value.clone(),
            data_type,
        }
    };

    match data {
        NorthwardData::Telemetry(t) => {
            let ts_ms = t.timestamp.timestamp_millis();
            t.values.iter().map(|pv| mk_row(ts_ms, pv)).collect()
        }
        NorthwardData::Attributes(a) => {
            let ts_ms = a.timestamp.timestamp_millis();
            a.client_attributes
                .iter()
                .chain(a.shared_attributes.iter())
                .chain(a.server_attributes.iter())
                .map(|pv| mk_row(ts_ms, pv))
                .collect()
        }
        _ => vec![],
    }
}
