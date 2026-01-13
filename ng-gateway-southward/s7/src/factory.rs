use crate::types::S7Channel;

use super::{
    driver::S7Driver,
    protocol::frame::parse_s7_address,
    types::{S7Action, S7Device, S7Parameter, S7Point},
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    ActionModel, DeviceModel, Driver, DriverError, DriverFactory, PointModel, RuntimeAction,
    RuntimeDevice, RuntimePoint, SouthwardInitContext,
};
use ng_gateway_sdk::{ChannelModel, DriverResult, RuntimeChannel};
use tracing::info;
// no local Serialize/Deserialize derives in this file
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct S7DriverFactory;

#[async_trait]
impl DriverFactory for S7DriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        Ok(Box::new(S7Driver::with_context(ctx)?))
    }

    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting S7 channel: {:?}", channel);

        Ok(Arc::new(S7Channel {
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
        Ok(Arc::new(S7Device {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        Ok(Arc::new(S7Point {
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
            address: point
                .driver_config
                .get("address")
                .and_then(|v| v.as_str())
                .map(parse_s7_address)
                .ok_or(DriverError::ConfigurationError(
                    "address is required for S7 parameter".to_string(),
                ))?
                .map_err(|e| DriverError::ConfigurationError(e.to_string()))?,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let inputs = action
            .inputs
            .into_iter()
            .map(|input| {
                Ok(S7Parameter {
                    name: input.name.clone(),
                    key: input.key,
                    data_type: input.data_type,
                    required: input.required,
                    default_value: input.default_value,
                    max_value: input.max_value,
                    min_value: input.min_value,
                    address: input
                        .driver_config
                        .get("address")
                        .and_then(|v| v.as_str())
                        .map(parse_s7_address)
                        .ok_or(DriverError::ConfigurationError(format!(
                            "address is invalid for S7 parameter: {}",
                            input.name
                        )))?
                        .map_err(|e| DriverError::ConfigurationError(e.to_string()))?,
                })
            })
            .collect::<DriverResult<Vec<S7Parameter>>>()?;
        Ok(Arc::new(S7Action {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters: inputs,
        }))
    }
}
