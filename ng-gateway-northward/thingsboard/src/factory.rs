use super::{config::ThingsBoardPluginConfig, plugin::ThingsBoardPlugin};
use async_trait::async_trait;
use ng_gateway_sdk::{
    NorthwardError, NorthwardInitContext, NorthwardResult, Plugin, PluginConfig, PluginFactory,
};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct ThingsBoardPluginFactory;

#[async_trait]
impl PluginFactory for ThingsBoardPluginFactory {
    fn create_plugin(&self, ctx: NorthwardInitContext) -> NorthwardResult<Box<dyn Plugin>> {
        Ok(Box::new(ThingsBoardPlugin::with_ctx(ctx)?))
    }

    fn convert_plugin_config(
        &self,
        config: serde_json::Value,
    ) -> NorthwardResult<Arc<dyn PluginConfig>> {
        let config: ThingsBoardPluginConfig =
            serde_json::from_value(config).map_err(|e| NorthwardError::SerializationError {
                reason: e.to_string(),
            })?;
        Ok(Arc::new(config))
    }
}
