//! Plugin factory for the Kafka northward adapter.

use super::{config::KafkaPluginConfig, plugin::KafkaPlugin};
use async_trait::async_trait;
use ng_gateway_sdk::{
    NorthwardError, NorthwardInitContext, NorthwardResult, Plugin, PluginConfig, PluginFactory,
};
use std::sync::Arc;

/// Default Kafka plugin factory.
#[derive(Debug, Clone, Default)]
pub struct KafkaPluginFactory;

#[async_trait]
impl PluginFactory for KafkaPluginFactory {
    fn create_plugin(&self, ctx: NorthwardInitContext) -> NorthwardResult<Box<dyn Plugin>> {
        Ok(Box::new(KafkaPlugin::with_ctx(ctx)?))
    }

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
