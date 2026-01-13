use crate::types::ModbusChannel;

use super::{
    driver::ModbusDriver,
    types::{ModbusAction, ModbusDevice, ModbusFunctionCode, ModbusParameter, ModbusPoint},
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    ActionModel, ChannelModel, DeviceModel, Driver, DriverError, DriverFactory, DriverResult,
    PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint, SouthwardInitContext,
};
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone, Default)]
pub struct ModbusDriverFactory;

#[async_trait]
impl DriverFactory for ModbusDriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        Ok(Box::new(ModbusDriver::with_context(ctx)?))
    }

    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting Modbus channel: {:?}", channel);

        Ok(Arc::new(ModbusChannel {
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
                    "Failed to deserialize ModbusChannelConfig: {e}"
                ))
            })?,
        }))
    }

    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>> {
        let driver_config = device.driver_config.ok_or(DriverError::ConfigurationError(
            "Driver config is required for Modbus device".to_string(),
        ))?;
        let slave_id = driver_config
            .get("slaveId")
            .ok_or(DriverError::ConfigurationError(
                "Slave ID is required for Modbus device".to_string(),
            ))?
            .as_u64()
            .map(|x| x as u8)
            .ok_or(DriverError::ConfigurationError(
                "Slave ID must be a number".to_string(),
            ))?;
        Ok(Arc::new(ModbusDevice {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
            slave_id,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let driver_config = point.driver_config;
        let mp = Self::extract_driver_config(driver_config).map(
            |(function_code, address, quantity)| ModbusPoint {
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
                function_code,
                address,
                quantity,
            },
        )?;
        Ok(Arc::new(mp))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let inputs = action
            .inputs
            .into_iter()
            .map(|input| {
                let driver_config = input.driver_config;
                Self::extract_driver_config(driver_config).map(
                    |(function_code, address, quantity)| ModbusParameter {
                        name: input.name,
                        key: input.key,
                        data_type: input.data_type,
                        required: input.required,
                        default_value: input.default_value,
                        max_value: input.max_value,
                        min_value: input.min_value,
                        function_code,
                        address,
                        quantity,
                    },
                )
            })
            .collect::<DriverResult<Vec<ModbusParameter>>>()?;
        Ok(Arc::new(ModbusAction {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters: inputs,
        }))
    }
}

impl ModbusDriverFactory {
    fn extract_driver_config(
        config: serde_json::Value,
    ) -> DriverResult<(ModbusFunctionCode, u16, u16)> {
        let function_code: ModbusFunctionCode = config
            .get("functionCode")
            .ok_or(DriverError::ConfigurationError(
                "Function code is required".into(),
            ))?
            .as_u64()
            .map(|x| x as u8)
            .ok_or(DriverError::ConfigurationError(
                "Function code must be a number".into(),
            ))?
            .try_into()?;
        let address = config
            .get("address")
            .ok_or(DriverError::ConfigurationError(
                "Address is required".into(),
            ))?
            .as_u64()
            .map(|x| x as u16)
            .ok_or(DriverError::ConfigurationError(
                "Address must be a number".into(),
            ))?;
        let quantity = config
            .get("quantity")
            .ok_or(DriverError::ConfigurationError(
                "Quantity is required".into(),
            ))?
            .as_u64()
            .map(|x| x as u16)
            .ok_or(DriverError::ConfigurationError(
                "Quantity must be a number".into(),
            ))?;
        Ok((function_code, address, quantity))
    }
}
