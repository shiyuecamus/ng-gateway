use crate::{
    protocol::frame::{encode_address_from_str, parse_di_str},
    types::{
        parse_di_from_config, Dl645Action, Dl645Channel, Dl645Device, Dl645FunctionCode,
        Dl645Parameter, Dl645Point,
    },
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    ActionModel, ChannelModel, DeviceModel, Driver, DriverError, DriverFactory, DriverResult,
    PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint, SouthwardInitContext,
};
use std::sync::Arc;
use tracing::info;

use crate::driver::Dl645Driver;

/// Factory for DL/T 645 drivers.
///
/// The factory is responsible for constructing driver instances and converting
/// persisted models into runtime structures.
#[derive(Debug, Clone, Default)]
pub struct Dl645DriverFactory;

#[async_trait]
impl DriverFactory for Dl645DriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        Ok(Box::new(Dl645Driver::with_context(ctx)?))
    }

    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting DL/T 645 channel: {:?}", channel);

        Ok(Arc::new(Dl645Channel {
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
                    "Failed to deserialize Dl645ChannelConfig: {e}"
                ))
            })?,
        }))
    }

    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>> {
        let driver_config = device
            .driver_config
            .unwrap_or_else(|| serde_json::json!({}));
        let address = driver_config
            .get("address")
            .and_then(|v| v.as_str())
            .ok_or(DriverError::ConfigurationError(
                "Device address is required for DL/T 645 device".to_string(),
            ))?
            .to_string();
        let address = encode_address_from_str(&address).map_err(|e| {
            DriverError::ConfigurationError(format!("Invalid DL/T 645 address {}: {e}", address))
        })?;

        let password = driver_config
            .get("password")
            .and_then(|v| v.as_str())
            .ok_or(DriverError::ConfigurationError(
                "Device password is required for DL/T 645 device".to_string(),
            ))?
            .to_string();

        let operator_code = driver_config
            .get("operatorCode")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());

        Ok(Arc::new(Dl645Device {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
            address,
            password,
            operator_code,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let driver_config = point.driver_config;
        let di = parse_di_from_config(&driver_config)?;
        let decimals = driver_config
            .get("decimals")
            .and_then(|v| v.as_u64())
            .map(|v| v as u8);

        Ok(Arc::new(Dl645Point {
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
            di,
            decimals,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let inputs = action
            .inputs
            .into_iter()
            .map(|input| {
                let driver_config = input.driver_config;
                let di = driver_config
                    .get("di")
                    .and_then(|v| v.as_str())
                    .map(|v| {
                        parse_di_str(v).ok_or(DriverError::ConfigurationError(format!(
                            "Invalid DI for DL/T 645 parameter: {}",
                            v
                        )))
                    })
                    .transpose()?;
                let decimals = driver_config
                    .get("decimals")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u8);
                let function_code: Dl645FunctionCode = driver_config
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
                Ok(Dl645Parameter {
                    name: input.name,
                    key: input.key,
                    data_type: input.data_type,
                    required: input.required,
                    default_value: input.default_value,
                    max_value: input.max_value,
                    min_value: input.min_value,
                    function_code,
                    di,
                    decimals,
                })
            })
            .collect::<DriverResult<Vec<Dl645Parameter>>>()?;

        Ok(Arc::new(Dl645Action {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters: inputs,
        }))
    }
}
