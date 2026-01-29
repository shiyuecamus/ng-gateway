//! Pulsar model conversion implementation (low-frequency path).
//!
//! This module implements `NorthwardModelConverter` for Pulsar, converting JSON config into a
//! typed, downcastable `PluginConfig` object. This MUST be deterministic and MUST NOT perform
//! any network or blocking I/O.

use super::config::PulsarPluginConfig;
use ng_gateway_sdk::{
    supervision::converter::NorthwardModelConverter, NorthwardError, NorthwardResult, PluginConfig,
};
use std::sync::Arc;

/// Pulsar default model converter.
#[derive(Debug, Clone, Default)]
pub struct PulsarConverter;

impl NorthwardModelConverter for PulsarConverter {
    fn convert_plugin_config(
        &self,
        config: serde_json::Value,
    ) -> NorthwardResult<Arc<dyn PluginConfig>> {
        let config: PulsarPluginConfig =
            serde_json::from_value(config).map_err(|e| NorthwardError::SerializationError {
                reason: e.to_string(),
            })?;
        Ok(Arc::new(config))
    }
}
