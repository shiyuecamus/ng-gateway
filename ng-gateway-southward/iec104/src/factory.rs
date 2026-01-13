use super::{
    driver::Iec104Driver,
    types::{Iec104Action, Iec104Channel, Iec104Device, Iec104Parameter, Iec104Point},
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    ActionModel, ChannelModel, DeviceModel, Driver, DriverError, DriverFactory, DriverResult,
    PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint, SouthwardInitContext,
};
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone, Default)]
pub struct Iec104DriverFactory;

#[async_trait]
impl DriverFactory for Iec104DriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        Ok(Box::new(Iec104Driver::with_context(ctx)?))
    }

    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting IEC104 channel: {:?}", channel);

        Ok(Arc::new(Iec104Channel {
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
                    "Failed to deserialize Iec104ChannelConfig: {e}"
                ))
            })?,
        }))
    }

    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>> {
        let driver_config = device.driver_config.ok_or(DriverError::ConfigurationError(
            "Driver config is required for IEC104 device".to_string(),
        ))?;
        let ca = driver_config.get("ca").and_then(|v| v.as_u64()).ok_or(
            DriverError::ConfigurationError("ca is required for IEC104 device".to_string()),
        )? as u16;
        Ok(Arc::new(Iec104Device {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
            ca,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let driver_config = point.driver_config;
        let ioa = driver_config.get("ioa").and_then(|v| v.as_u64()).ok_or(
            DriverError::ConfigurationError("ioa is required for IEC104 point".to_string()),
        )? as u16;
        let type_id = driver_config.get("typeId").and_then(|v| v.as_u64()).ok_or(
            DriverError::ConfigurationError("typeId is required for IEC104 point".to_string()),
        )? as u8;
        Ok(Arc::new(Iec104Point {
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
            ioa,
            type_id,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let inputs = action
            .inputs
            .into_iter()
            .map(|input| {
                let driver_config = input.driver_config;
                let ioa = driver_config.get("ioa").and_then(|v| v.as_u64()).ok_or(
                    DriverError::ConfigurationError("ioa is required for IEC104 input".to_string()),
                )? as u16;
                let type_id = driver_config.get("typeId").and_then(|v| v.as_u64()).ok_or(
                    DriverError::ConfigurationError(
                        "typeId is required for IEC104 input".to_string(),
                    ),
                )? as u8;
                Ok(Iec104Parameter {
                    name: input.name,
                    key: input.key,
                    data_type: input.data_type,
                    required: input.required,
                    default_value: input.default_value,
                    max_value: input.max_value,
                    min_value: input.min_value,
                    ioa,
                    type_id,
                })
            })
            .collect::<DriverResult<Vec<Iec104Parameter>>>()?;
        Ok(Arc::new(Iec104Action {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters: inputs,
        }))
    }
}
