use crate::types::OpcUaChannel;

use super::{
    driver::OpcUaDriver,
    types::{OpcUaAction, OpcUaDevice, OpcUaParameter, OpcUaPoint},
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    ActionModel, ChannelModel, DeviceModel, Driver, DriverError, DriverFactory, DriverResult,
    PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint, SouthwardInitContext,
};
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone, Default)]
pub struct OpcUaDriverFactory;

#[async_trait]
impl DriverFactory for OpcUaDriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        Ok(Box::new(OpcUaDriver::with_context(ctx)?))
    }

    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting OPC UA channel: {:?}", channel);

        Ok(Arc::new(OpcUaChannel {
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
                    "Failed to deserialize OpcUaChannelConfig: {e}"
                ))
            })?,
        }))
    }

    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>> {
        Ok(Arc::new(OpcUaDevice {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let driver_config = point.driver_config;
        let node_id = driver_config
            .get("nodeId")
            .and_then(|v| v.as_str())
            .ok_or(DriverError::ConfigurationError(
                "nodeId is required for OPC UA point".to_string(),
            ))?
            .to_string();
        Ok(Arc::new(OpcUaPoint {
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
            node_id,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let inputs = action
            .inputs
            .into_iter()
            .map(|input| {
                let driver_config = input.driver_config;
                let node_id = driver_config
                    .get("nodeId")
                    .and_then(|v| v.as_str())
                    .ok_or(DriverError::ConfigurationError(
                        "nodeId is required for OPC UA input".to_string(),
                    ))?
                    .to_string();
                Ok(OpcUaParameter {
                    name: input.name,
                    key: input.key,
                    data_type: input.data_type,
                    required: input.required,
                    default_value: input.default_value,
                    max_value: input.max_value,
                    min_value: input.min_value,
                    node_id,
                })
            })
            .collect::<DriverResult<Vec<OpcUaParameter>>>()?;
        Ok(Arc::new(OpcUaAction {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters: inputs,
        }))
    }
}
