//! Model converter for the Modbus driver.
//!
//! Converts DB models (`ChannelModel`, `DeviceModel`, `PointModel`, `ActionModel`) into typed
//! runtime objects used by the driver implementation.
//!
//! IMPORTANT:
//! - Conversions MUST be deterministic.
//! - Conversions MUST NOT perform any network or blocking I/O.

use super::types::{
    ModbusAction, ModbusChannel, ModbusDevice, ModbusFunctionCode, ModbusParameter, ModbusPoint,
};
use ng_gateway_sdk::{
    supervision::converter::SouthwardModelConverter, ActionModel, ChannelModel, DeviceModel,
    DriverError, DriverResult, PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice,
    RuntimePoint,
};
use std::sync::Arc;
use tracing::info;

/// Default model converter for the Modbus driver.
#[derive(Debug, Clone, Default)]
pub struct ModbusConverter;

impl ModbusConverter {
    fn parse_function_code(v: &serde_json::Value) -> DriverResult<ModbusFunctionCode> {
        let code = v.as_u64().ok_or(DriverError::ConfigurationError(
            "functionCode must be a number".to_string(),
        ))?;
        let code_u8 = u8::try_from(code).map_err(|_| {
            DriverError::ConfigurationError("functionCode out of range (0..=255)".to_string())
        })?;
        ModbusFunctionCode::try_from(code_u8)
    }

    fn extract_point_driver_config(
        driver_config: serde_json::Value,
    ) -> DriverResult<(ModbusFunctionCode, u16, u16)> {
        let function_code = driver_config
            .get("functionCode")
            .ok_or(DriverError::ConfigurationError(
                "functionCode is required".to_string(),
            ))
            .and_then(Self::parse_function_code)?;

        let address = driver_config
            .get("address")
            .and_then(|v| v.as_u64())
            .ok_or(DriverError::ConfigurationError(
                "address is required".to_string(),
            ))
            .and_then(|v| {
                u16::try_from(v).map_err(|_| {
                    DriverError::ConfigurationError("address out of range".to_string())
                })
            })?;

        let quantity = driver_config
            .get("quantity")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        let quantity = u16::try_from(quantity)
            .map_err(|_| DriverError::ConfigurationError("quantity out of range".to_string()))?
            .max(1);

        Ok((function_code, address, quantity))
    }
}

impl SouthwardModelConverter for ModbusConverter {
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
            .and_then(|v| v.as_u64())
            .and_then(|v| u8::try_from(v).ok())
            .ok_or(DriverError::ConfigurationError(
                "Slave ID is required for Modbus device".to_string(),
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
        let (function_code, address, quantity) =
            Self::extract_point_driver_config(point.driver_config)?;
        Ok(Arc::new(ModbusPoint {
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
            transform: point.transform,
            function_code,
            address,
            quantity,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let input_parameters = action
            .inputs
            .into_iter()
            .map(|p| {
                let driver_config = p.driver_config;
                let function_code = driver_config
                    .get("functionCode")
                    .ok_or(DriverError::ConfigurationError(
                        "functionCode is required".to_string(),
                    ))
                    .and_then(Self::parse_function_code)?;
                let address = driver_config
                    .get("address")
                    .and_then(|v| v.as_u64())
                    .ok_or(DriverError::ConfigurationError(
                        "address is required".to_string(),
                    ))
                    .and_then(|v| {
                        u16::try_from(v).map_err(|_| {
                            DriverError::ConfigurationError("address out of range".to_string())
                        })
                    })?;
                let quantity = driver_config
                    .get("quantity")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                let quantity = u16::try_from(quantity)
                    .map_err(|_| {
                        DriverError::ConfigurationError("quantity out of range".to_string())
                    })?
                    .max(1);

                Ok(ModbusParameter {
                    name: p.name,
                    key: p.key,
                    data_type: p.data_type,
                    required: p.required,
                    default_value: p.default_value,
                    max_value: p.max_value,
                    min_value: p.min_value,
                    transform: p.transform,
                    function_code,
                    address,
                    quantity,
                })
            })
            .collect::<DriverResult<Vec<_>>>()?;

        Ok(Arc::new(ModbusAction {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters,
        }))
    }
}
