use crate::{DataType, DriverError, DriverResult, NGValue, RuntimeParameter};
use std::sync::Arc;

/// Validate action parameters including required presence and numeric ranges.
///
/// This is the canonical validation used across the gateway and all drivers.
/// It ensures that required parameters exist with the correct container shape
/// and validates numeric values against `min_value` and `max_value` when
/// applicable. Error messages are human-readable and intended for RPC responses.
pub fn validate_action_parameters(
    inputs: &[Arc<dyn RuntimeParameter>],
    params: &Option<serde_json::Value>,
) -> DriverResult<()> {
    let params_value = params.as_ref();
    let params_obj = params_value.and_then(|v| v.as_object());
    let inputs_len = inputs.len();

    // Required presence check
    let required_keys: Vec<String> = inputs
        .iter()
        .filter(|input| input.required())
        .map(|input| input.key().to_string())
        .collect();

    if !required_keys.is_empty() {
        if required_keys.len() == 1 {
            if params_value.is_none() {
                return Err(DriverError::ValidationError(
                    "Parameters is required for required parameters".to_string(),
                ));
            }
            return Ok(());
        } else if params_obj.is_none() {
            return Err(DriverError::ValidationError(
                "Parameters must be a JSON object for required parameters".to_string(),
            ));
        }
    }

    let missing_required: Vec<String> = match params_obj {
        Some(map) => required_keys
            .iter()
            .filter(|key| !map.contains_key(key.as_str()))
            .cloned()
            .collect(),
        None => {
            if inputs_len == 1 {
                match inputs.first() {
                    Some(input) if input.required() && params_value.is_none() => {
                        vec![input.key().to_string()]
                    }
                    _ => Vec::new(),
                }
            } else {
                required_keys
            }
        }
    };

    if !missing_required.is_empty() {
        return Err(DriverError::ValidationError(format!(
            "Missing required parameters: {}",
            missing_required.join(",")
        )));
    }

    // Numeric range validation
    let mut errors: Vec<String> = Vec::new();
    for input in inputs.iter() {
        let key = input.key();
        let value_opt: Option<&serde_json::Value> = match params_obj {
            Some(map) => map.get(key),
            None => {
                if inputs_len == 1 {
                    params_value
                } else {
                    None
                }
            }
        };

        let Some(value) = value_opt else { continue };

        // Numeric-like types only
        let is_numeric = matches!(
            input.data_type(),
            DataType::Int8
                | DataType::UInt8
                | DataType::Int16
                | DataType::UInt16
                | DataType::Int32
                | DataType::UInt32
                | DataType::Int64
                | DataType::UInt64
                | DataType::Float32
                | DataType::Float64
                | DataType::Timestamp
        );
        if !is_numeric {
            continue;
        }

        let numeric_value: Option<f64> = value
            .as_f64()
            .or_else(|| value.as_i64().map(|v| v as f64))
            .or_else(|| value.as_u64().map(|v| v as f64))
            .or_else(|| value.as_str().and_then(|s| s.parse::<f64>().ok()));

        match numeric_value {
            Some(v) => {
                if let Some(min) = input.min_value() {
                    if v < min {
                        errors.push(format!("{key} < min_value ({min})"));
                    }
                }
                if let Some(max) = input.max_value() {
                    if v > max {
                        errors.push(format!("{key} > max_value ({max})"));
                    }
                }
            }
            None => {
                errors.push(format!("{key} is not a numeric value"));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(DriverError::ValidationError(format!(
            "Parameter validation failed: {}",
            errors.join("; ")
        )))
    }
}

/// Resolve input parameter values with defaulting semantics (unchecked variant).
///
/// This function assumes the caller has validated presence and container shape.
/// Optional parameters must supply `default_value()`, otherwise this function
/// returns a `ConfigurationError`.
pub fn resolve_action_inputs_typed<P>(
    inputs: &[Arc<dyn RuntimeParameter>],
    parameters: &Option<serde_json::Value>,
) -> DriverResult<Vec<(Arc<P>, serde_json::Value)>>
where
    P: RuntimeParameter + 'static,
{
    let total = inputs.len();
    let mut out: Vec<(Arc<P>, serde_json::Value)> = Vec::with_capacity(total);

    let required_keys: Vec<String> = inputs
        .iter()
        .filter(|input| input.required())
        .map(|input| input.key().to_string())
        .collect();

    let params_obj = parameters.as_ref().and_then(|v| v.as_object());

    for param in inputs.iter() {
        let required = param.required();
        let value: serde_json::Value = if !required {
            match params_obj {
                Some(map) => match map.get(param.key()) {
                    Some(v) => v.clone(),
                    None => match param.default_value() {
                        Some(v) => v,
                        None => {
                            return Err(DriverError::ConfigurationError(format!(
                                "Optional parameter '{}' missing default value",
                                param.key()
                            )));
                        }
                    },
                },
                None => match param.default_value() {
                    Some(v) => v,
                    None => {
                        return Err(DriverError::ConfigurationError(format!(
                            "Optional parameter '{}' missing default value",
                            param.key()
                        )));
                    }
                },
            }
        } else if total == 1 || required_keys.len() == 1 {
            match params_obj {
                Some(map) => match map.get(param.key()) {
                    Some(v) => v.clone(),
                    None => match parameters.as_ref() {
                        Some(v) => v.clone(),
                        None => {
                            return Err(DriverError::ConfigurationError(
                                "Missing parameters for required input".to_string(),
                            ));
                        }
                    },
                },
                None => match parameters.as_ref() {
                    Some(v) => v.clone(),
                    None => {
                        return Err(DriverError::ConfigurationError(
                            "Missing parameters for required input".to_string(),
                        ));
                    }
                },
            }
        } else {
            match params_obj.and_then(|m| m.get(param.key())) {
                Some(v) => v.clone(),
                None => {
                    return Err(DriverError::ConfigurationError(format!(
                        "Missing value for required parameter '{}'",
                        param.key()
                    )));
                }
            }
        };

        let p = match Arc::clone(param).downcast_arc::<P>() {
            Ok(p) => p,
            Err(_) => {
                return Err(DriverError::ConfigurationError(format!(
                    "Parameter type mismatch for key '{}' (expected {})",
                    param.key(),
                    std::any::type_name::<P>()
                )));
            }
        };

        out.push((p, value));
    }

    Ok(out)
}

/// Validate, default and resolve action inputs into strongly-typed `NGValue`s.
///
/// This is the **single gateway entrypoint** for execute_action input handling:
/// - Compatible shape semantics:
///   - multi-parameter: `params` must be JSON object
///   - single-parameter: allow scalar or object containing the key
/// - Required checks and default-value fill
/// - Lenient scalar conversion (numeric strings, 0x hex, bool strings, RFC3339 timestamps, base64/hex binary)
/// - Stable output order: follows `inputs` order
pub fn validate_and_resolve_action_inputs(
    inputs: &[Arc<dyn RuntimeParameter>],
    params: &Option<serde_json::Value>,
) -> DriverResult<Vec<(Arc<dyn RuntimeParameter>, NGValue)>> {
    let total = inputs.len();
    let mut out: Vec<(Arc<dyn RuntimeParameter>, NGValue)> = Vec::with_capacity(total);

    let params_value = params.as_ref();
    let params_obj = params_value.and_then(|v| v.as_object());

    // Reuse existing required presence semantics to preserve behavior.
    let required_keys: Vec<String> = inputs
        .iter()
        .filter(|input| input.required())
        .map(|input| input.key().to_string())
        .collect();

    if !required_keys.is_empty() {
        if required_keys.len() == 1 {
            if params_value.is_none() {
                return Err(DriverError::ValidationError(
                    "Parameters is required for required parameters".to_string(),
                ));
            }
        } else if params_obj.is_none() {
            return Err(DriverError::ValidationError(
                "Parameters must be a JSON object for required parameters".to_string(),
            ));
        }
    }

    let missing_required: Vec<String> = match params_obj {
        Some(map) => required_keys
            .iter()
            .filter(|key| !map.contains_key(key.as_str()))
            .cloned()
            .collect(),
        None => {
            if total == 1 {
                match inputs.first() {
                    Some(input) if input.required() && params_value.is_none() => {
                        vec![input.key().to_string()]
                    }
                    _ => Vec::new(),
                }
            } else {
                required_keys.clone()
            }
        }
    };

    if !missing_required.is_empty() {
        return Err(DriverError::ValidationError(format!(
            "Missing required parameters: {}",
            missing_required.join(",")
        )));
    }

    // Range validation is aggregated to preserve error style.
    let mut range_errors: Vec<String> = Vec::new();

    for param in inputs.iter() {
        let required = param.required();
        let key = param.key();

        // Resolve JSON source value (borrowed) or default (owned for optional).
        let value_ref: Option<&serde_json::Value> = params_obj.and_then(|map| map.get(key));
        let expected = param.data_type();

        let ng: NGValue = if !required {
            match value_ref {
                Some(v) => {
                    if v.is_null() {
                        return Err(DriverError::ValidationError(format!(
                            "Parameter '{}' is null",
                            key
                        )));
                    }
                    if v.is_array() || v.is_object() {
                        return Err(DriverError::ValidationError(format!(
                            "Parameter '{}' must be a JSON scalar",
                            key
                        )));
                    }
                    NGValue::try_from_json_scalar(expected, v).ok_or(
                        DriverError::ValidationError(format!(
                            "Parameter '{}' type conversion failed (expected {:?})",
                            key, expected
                        )),
                    )?
                }
                None => {
                    let dv = param
                        .default_value()
                        .ok_or(DriverError::ConfigurationError(format!(
                            "Optional parameter '{}' missing default value",
                            key
                        )))?;
                    if dv.is_null() {
                        return Err(DriverError::ValidationError(format!(
                            "Parameter '{}' default value is null",
                            key
                        )));
                    }
                    if dv.is_array() || dv.is_object() {
                        return Err(DriverError::ValidationError(format!(
                            "Parameter '{}' default value must be a JSON scalar",
                            key
                        )));
                    }
                    NGValue::try_from_json_scalar(expected, &dv).ok_or(
                        DriverError::ValidationError(format!(
                            "Parameter '{}' type conversion failed (expected {:?})",
                            key, expected
                        )),
                    )?
                }
            }
        } else {
            let chosen: &serde_json::Value = if total == 1 || required_keys.len() == 1 {
                match params_obj {
                    Some(map) => match map.get(key) {
                        Some(v) => v,
                        None => params_value.ok_or(DriverError::ConfigurationError(
                            "Missing parameters for required input".to_string(),
                        ))?,
                    },
                    None => params_value.ok_or(DriverError::ConfigurationError(
                        "Missing parameters for required input".to_string(),
                    ))?,
                }
            } else {
                params_obj
                    .and_then(|m| m.get(key))
                    .ok_or(DriverError::ConfigurationError(format!(
                        "Missing value for required parameter '{}'",
                        key
                    )))?
            };

            if chosen.is_null() {
                return Err(DriverError::ValidationError(format!(
                    "Parameter '{}' is null",
                    key
                )));
            }
            if chosen.is_array() || chosen.is_object() {
                return Err(DriverError::ValidationError(format!(
                    "Parameter '{}' must be a JSON scalar",
                    key
                )));
            }
            NGValue::try_from_json_scalar(expected, chosen).ok_or(DriverError::ValidationError(
                format!(
                    "Parameter '{}' type conversion failed (expected {:?})",
                    key, expected
                ),
            ))?
        };

        // Numeric range checks (only when declared on the parameter).
        let need_range = param.min_value().is_some() || param.max_value().is_some();
        if need_range {
            let numeric_value: Option<f64> = match &ng {
                NGValue::Int8(v) => Some(*v as f64),
                NGValue::UInt8(v) => Some(*v as f64),
                NGValue::Int16(v) => Some(*v as f64),
                NGValue::UInt16(v) => Some(*v as f64),
                NGValue::Int32(v) => Some(*v as f64),
                NGValue::UInt32(v) => Some(*v as f64),
                NGValue::Int64(v) => Some(*v as f64),
                NGValue::UInt64(v) => Some(*v as f64),
                NGValue::Float32(v) => Some(*v as f64),
                NGValue::Float64(v) => Some(*v),
                NGValue::Timestamp(ms) => Some(*ms as f64),
                _ => None,
            };

            if let Some(v) = numeric_value {
                if let Some(min) = param.min_value() {
                    if v < min {
                        range_errors.push(format!("{key} < min_value ({min})"));
                    }
                }
                if let Some(max) = param.max_value() {
                    if v > max {
                        range_errors.push(format!("{key} > max_value ({max})"));
                    }
                }
            }
        }

        out.push((Arc::clone(param), ng));
    }

    if range_errors.is_empty() {
        Ok(out)
    } else {
        Err(DriverError::ValidationError(format!(
            "Parameter validation failed: {}",
            range_errors.join("; ")
        )))
    }
}

/// Downcast resolved parameters to a concrete protocol parameter type.
pub fn downcast_parameters<P: RuntimeParameter + 'static>(
    params: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
) -> DriverResult<Vec<(Arc<P>, NGValue)>> {
    let mut out: Vec<(Arc<P>, NGValue)> = Vec::with_capacity(params.len());
    for (p, v) in params.into_iter() {
        let key = p.key().to_string();
        let p = p.downcast_arc::<P>().map_err(|_| {
            DriverError::ConfigurationError(format!(
                "Parameter type mismatch for key '{}' (expected {})",
                key,
                std::any::type_name::<P>()
            ))
        })?;
        out.push((p, v));
    }
    Ok(out)
}
