//! Mapping engine for declarative JSON transformations.
//!
//! This module is designed to be shared by all northward plugins (MQTT/HTTP/Kafka/Pulsar...).
//! The goal is to provide a **single** canonical implementation of `mapped_json`:
//! - Compile expressions once, apply many times
//! - Pre-parse output paths to minimize per-message overhead
//! - Provide a stable input view for mapping expressions (see `build_mapping_input`)
//!
//! Feature gate:
//! - This module depends on `jmespath` and is intended for async runtimes.

use crate::{envelope::EnvelopeKind, northward::NorthwardData};
use jmespath::Expression;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

/// One mapping rule: write `expr(input)` into `out_path`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappedRule {
    /// Dot-separated output path (e.g. `payload.data.point_id`).
    pub out_path: String,
    /// JMESPath expression evaluated against the input JSON.
    pub expr: String,
}

/// Mapping spec as a list of rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappedJsonSpec {
    /// Ordered rules (stable evaluation order).
    #[serde(default)]
    pub rules: Vec<MappedRule>,
}

impl From<BTreeMap<String, String>> for MappedJsonSpec {
    fn from(map: BTreeMap<String, String>) -> Self {
        let rules = map
            .into_iter()
            .map(|(out_path, expr)| MappedRule { out_path, expr })
            .collect();
        Self { rules }
    }
}

/// Conflict policy for output-path writes.
#[derive(Debug, Clone, Copy, Default)]
pub enum OutPathConflictPolicy {
    /// Overwrite conflicting nodes to keep mapping tolerant (default).
    #[default]
    Overwrite,
    /// Fail on type conflicts (recommended for strict pipelines).
    Error,
}

/// Mapping compilation/execution errors.
#[derive(Debug, Error)]
pub enum MappingError {
    /// Rule is malformed (e.g. empty out_path or expr).
    #[error("invalid rule: {0}")]
    InvalidRule(String),
    /// Expression failed to compile.
    #[error("compile failed (expr={expr}): {error}")]
    Compile { expr: String, error: String },
    /// Expression failed to evaluate on input.
    #[error("eval failed (expr={expr}): {error}")]
    Eval { expr: String, error: String },
    /// Output path write conflict.
    #[error("out_path conflict (path={path}): {error}")]
    OutPath { path: String, error: String },
}

/// A compiled mapping ready for hot-path apply.
#[derive(Debug, Clone)]
pub struct CompiledMappedJson {
    rules: Arc<[CompiledRule]>,
    conflict_policy: OutPathConflictPolicy,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    out_path: String,
    segments: Arc<[String]>,
    expr: String,
    compiled: Expression<'static>,
}

impl CompiledMappedJson {
    /// Compile a mapping spec into an executable mapping.
    pub fn compile(spec: &MappedJsonSpec) -> Result<Self, MappingError> {
        Self::compile_with_policy(spec, OutPathConflictPolicy::default())
    }

    /// Compile with a specific conflict policy.
    pub fn compile_with_policy(
        spec: &MappedJsonSpec,
        conflict_policy: OutPathConflictPolicy,
    ) -> Result<Self, MappingError> {
        let mut out: Vec<CompiledRule> = Vec::with_capacity(spec.rules.len());
        for r in spec.rules.iter() {
            let out_path = r.out_path.trim();
            let expr = r.expr.trim();
            if out_path.is_empty() {
                return Err(MappingError::InvalidRule(
                    "out_path must not be empty".to_string(),
                ));
            }
            if expr.is_empty() {
                return Err(MappingError::InvalidRule(format!(
                    "expr must not be empty (out_path={out_path})"
                )));
            }

            let segments: Vec<String> = out_path
                .split('.')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect();
            if segments.is_empty() {
                return Err(MappingError::InvalidRule(format!(
                    "out_path has no segments (out_path={out_path})"
                )));
            }

            let compiled = jmespath::compile(expr).map_err(|e| MappingError::Compile {
                expr: expr.to_string(),
                error: e.to_string(),
            })?;
            out.push(CompiledRule {
                out_path: out_path.to_string(),
                segments: Arc::from(segments.into_boxed_slice()),
                expr: expr.to_string(),
                compiled,
            });
        }

        Ok(Self {
            rules: Arc::from(out.into_boxed_slice()),
            conflict_policy,
        })
    }

    /// Apply the mapping to input JSON and return the generated JSON object.
    ///
    /// This is intended to be used on hot paths. It avoids allocations by reusing compiled
    /// expressions and pre-parsed out_path segments.
    pub fn apply(&self, input: &Value) -> Result<Value, MappingError> {
        let mut out = Value::Object(Map::new());
        for r in self.rules.iter() {
            let v = eval_compiled_expr(input, &r.compiled, r.expr.as_str())?;
            set_out_path_segments(
                &mut out,
                r.out_path.as_str(),
                r.segments.as_ref(),
                v,
                self.conflict_policy,
            )?;
        }
        Ok(out)
    }

    /// Apply mapping and return best-effort JSON output.
    ///
    /// Any evaluation/write errors are turned into `null` assignments or dropped writes,
    /// depending on the failure point. This is useful for tolerant pipelines.
    pub fn apply_lossy(&self, input: &Value) -> Value {
        let mut out = Value::Object(Map::new());
        for r in self.rules.iter() {
            let v = eval_compiled_expr(input, &r.compiled, r.expr.as_str()).unwrap_or(Value::Null);
            let _ = set_out_path_segments(
                &mut out,
                r.out_path.as_str(),
                r.segments.as_ref(),
                v,
                OutPathConflictPolicy::Overwrite,
            );
        }
        out
    }
}

fn eval_compiled_expr(
    input: &Value,
    compiled: &Expression<'static>,
    expr: &str,
) -> Result<Value, MappingError> {
    let result = compiled.search(input).map_err(|e| MappingError::Eval {
        expr: expr.to_string(),
        error: e.to_string(),
    })?;
    Ok(serde_json::to_value(result.as_ref()).unwrap_or(Value::Null))
}

/// Set a nested value using pre-parsed segments.
fn set_out_path_segments(
    root: &mut Value,
    out_path: &str,
    segments: &[String],
    value: Value,
    policy: OutPathConflictPolicy,
) -> Result<(), MappingError> {
    if segments.is_empty() {
        return Err(MappingError::InvalidRule(format!(
            "out_path has no segments (out_path={out_path})"
        )));
    }

    if !root.is_object() {
        *root = Value::Object(Map::new());
    }

    let mut cur = root;
    for (idx, seg) in segments.iter().enumerate() {
        let last = idx + 1 == segments.len();
        if last {
            match cur {
                Value::Object(m) => {
                    m.insert(seg.clone(), value);
                }
                _ => {
                    return Err(MappingError::OutPath {
                        path: out_path.to_string(),
                        error: "parent is not an object".to_string(),
                    });
                }
            }
            return Ok(());
        }

        match cur {
            Value::Object(m) => {
                let needs_new = !m.contains_key(seg) || !m.get(seg).is_some_and(|v| v.is_object());
                if needs_new {
                    match policy {
                        OutPathConflictPolicy::Overwrite => {
                            m.insert(seg.clone(), Value::Object(Map::new()));
                        }
                        OutPathConflictPolicy::Error => {
                            return Err(MappingError::OutPath {
                                path: out_path.to_string(),
                                error: format!("segment {seg} conflicts with non-object value"),
                            });
                        }
                    }
                }
                let Some(next) = m.get_mut(seg) else {
                    return Err(MappingError::OutPath {
                        path: out_path.to_string(),
                        error: format!("failed to access segment {seg}"),
                    });
                };
                cur = next;
            }
            _ => {
                return Err(MappingError::OutPath {
                    path: out_path.to_string(),
                    error: "root is not an object".to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Build the canonical mapping input JSON view for a `NorthwardData`.
///
/// JSON shape (stable):
/// `{ schema_version, event_kind, ts_ms, app:{...}, device:{...}, channel?, data:{...} }`
///
/// This is the **only** recommended input shape for `mapped_json` across all plugins.
pub fn build_mapping_input(
    plugin_type: &str,
    app_id: i32,
    app_name: &str,
    data: &NorthwardData,
) -> Value {
    #[derive(Debug, Serialize)]
    struct MappingApp {
        id: i32,
        name: String,
        plugin_type: String,
    }
    #[derive(Debug, Serialize)]
    struct MappingDevice {
        id: i32,
        name: String,
        #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
        r#type: Option<String>,
    }
    #[derive(Debug, Serialize)]
    struct MappingInput {
        schema_version: u32,
        event_kind: EnvelopeKind,
        ts_ms: i64,
        app: MappingApp,
        device: MappingDevice,
        data: Value,
    }

    let kind = data.envelope_kind();
    let ts_ms = match data {
        NorthwardData::DeviceConnected(_) | NorthwardData::DeviceDisconnected(_) => {
            chrono::Utc::now().timestamp_millis()
        }
        NorthwardData::Telemetry(t) => t.timestamp.timestamp_millis(),
        NorthwardData::Attributes(a) => a.timestamp.timestamp_millis(),
        NorthwardData::Alarm(a) => a.timestamp.timestamp_millis(),
        NorthwardData::RpcResponse(r) => r.timestamp.timestamp_millis(),
        NorthwardData::WritePointResponse(r) => r.completed_at.timestamp_millis(),
    };

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

    let data_v = serde_json::to_value(data).unwrap_or(Value::Null);
    serde_json::to_value(MappingInput {
        schema_version: 1,
        event_kind: kind,
        ts_ms,
        app: MappingApp {
            id: app_id,
            name: app_name.to_string(),
            plugin_type: plugin_type.to_string(),
        },
        device: MappingDevice {
            id: device_id,
            name: device_name,
            r#type: device_type,
        },
        data: data_v,
    })
    .unwrap_or(Value::Null)
}
