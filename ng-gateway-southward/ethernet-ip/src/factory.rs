use super::{
    driver::EthernetIpDriver,
    types::{
        EthernetIpAction, EthernetIpChannel, EthernetIpDevice, EthernetIpParameter, EthernetIpPoint,
    },
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    ActionModel, ChannelModel, DeviceModel, Driver, DriverError, DriverFactory, DriverResult,
    PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint, SouthwardInitContext,
};
use std::sync::Arc;
use tracing::info;

/// Ethernet/IP Driver Factory
#[derive(Debug, Clone, Copy, Default)]
pub struct EthernetIpDriverFactory;

#[async_trait]
impl DriverFactory for EthernetIpDriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        let driver = EthernetIpDriver::with_context(ctx)?;
        Ok(Box::new(driver))
    }

    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting Ethernet/IP channel: {:?}", channel);

        Ok(Arc::new(EthernetIpChannel {
            id: channel.id,
            name: channel.name,
            status: channel.status,
            driver_id: channel.driver_id,
            collection_type: channel.collection_type,
            report_type: channel.report_type,
            period: channel.period,
            connection_policy: channel.connection_policy,
            config: serde_json::from_value(channel.driver_config).map_err(|e| {
                DriverError::ConfigurationError(format!(
                    "Failed to deserialize EthernetIpChannelConfig: {e}"
                ))
            })?,
        }))
    }

    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>> {
        Ok(Arc::new(EthernetIpDevice {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let driver_config = point.driver_config;
        let tag_name = driver_config
            .get("tagName")
            .ok_or(DriverError::ConfigurationError(
                "tagName is required".into(),
            ))?
            .as_str()
            .ok_or(DriverError::ConfigurationError(
                "tagName must be a string".into(),
            ))?
            .to_string();

        Ok(Arc::new(EthernetIpPoint {
            id: point.id,
            device_id: point.device_id,
            name: point.name,
            key: point.key,
            r#type: point.r#type,
            data_type: point.data_type,
            access_mode: point.access_mode,
            unit: point.unit,
            min_value: point.min_value,
            max_value: point.max_value,
            scale: point.scale,
            tag_name,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let action_id = action.id;
        let command = action.command.clone();
        let inputs_len = action.inputs.len();

        let inputs = action
            .inputs
            .into_iter()
            .map(|input| -> DriverResult<EthernetIpParameter> {
                // Ethernet/IP protocol details MUST live in Parameter.driver_config, not Action.
                //
                // Compatibility note:
                // Older configurations might have encoded the target tag in Action.command.
                // To keep backward compatibility, we only fallback to Action.command when:
                // - this is a single-parameter action, AND
                // - Parameter.driver_config.tagName is missing or empty.
                //
                // New configurations MUST set `driver_config.tagName`.
                let tag_name_opt = input
                    .driver_config
                    .get("tagName")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());

                let tag_name = if let Some(v) = tag_name_opt {
                    v
                } else if inputs_len == 1 {
                    tracing::warn!(
                        action_id = action_id,
                        action_command = command,
                        parameter_key = input.key,
                        "Ethernet/IP action parameter is missing driver_config.tagName; falling back to action.command (deprecated)"
                    );
                    command.clone()
                } else {
                    return Err(DriverError::ConfigurationError(format!(
                        "tagName is required for Ethernet/IP action parameter '{}'",
                        input.name
                    )));
                };

                Ok(EthernetIpParameter {
                    name: input.name,
                    key: input.key,
                    data_type: input.data_type,
                    required: input.required,
                    default_value: input.default_value,
                    max_value: input.max_value,
                    min_value: input.min_value,
                    tag_name,
                })
            })
            .collect::<DriverResult<Vec<EthernetIpParameter>>>()?;

        Ok(Arc::new(EthernetIpAction {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command,
            input_parameters: inputs,
        }))
    }
}
