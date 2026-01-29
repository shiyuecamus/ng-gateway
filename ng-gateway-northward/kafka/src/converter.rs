//! Kafka model conversion implementation (low-frequency path).
//!
//! This module implements `NorthwardModelConverter` for Kafka, converting JSON config into a
//! typed, downcastable `PluginConfig` object. This MUST be deterministic and MUST NOT perform
//! any network or blocking I/O.

use super::config::KafkaPluginConfig;
use ng_gateway_sdk::{
    supervision::converter::NorthwardModelConverter, NorthwardError, NorthwardResult, PluginConfig,
};
use std::sync::Arc;

/// Kafka default model converter.
#[derive(Debug, Clone, Default)]
pub struct KafkaConverter;

impl NorthwardModelConverter for KafkaConverter {
    fn convert_plugin_config(
        &self,
        config: serde_json::Value,
    ) -> NorthwardResult<Arc<dyn PluginConfig>> {
        let config: KafkaPluginConfig =
            serde_json::from_value(config).map_err(|e| NorthwardError::SerializationError {
                reason: e.to_string(),
            })?;
        Ok(Arc::new(config))
    }
}
