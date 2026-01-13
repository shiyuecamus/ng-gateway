use super::{
    protocol::frame::addr::McLogicalAddress,
    types::{McAction, McAddress, McChannel, McChannelConfig, McDevice, McParameter, McPoint},
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    ActionModel, ChannelModel, DeviceModel, Driver, DriverError, DriverFactory, DriverResult,
    PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint, SouthwardInitContext,
};
use std::sync::Arc;
use tracing::info;

/// Driver factory for Mitsubishi MC protocol driver.
///
/// This factory is responsible for:
/// - Creating driver instances from initialization context
/// - Converting persisted models into runtime channel/device/point/action types
#[derive(Debug, Clone, Default)]
pub struct McDriverFactory;

#[async_trait]
impl DriverFactory for McDriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        // Construct a strongly-typed MC driver from initialization context.
        Ok(Box::new(crate::driver::McDriver::with_context(ctx)?))
    }

    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting MC channel: {:?}", channel);

        let config: McChannelConfig =
            serde_json::from_value(channel.driver_config).map_err(|e| {
                DriverError::ConfigurationError(format!(
                    "Failed to deserialize McChannelConfig: {e}"
                ))
            })?;

        Ok(Arc::new(McChannel {
            id: channel.id,
            name: channel.name,
            driver_id: channel.driver_id,
            collection_type: channel.collection_type,
            report_type: channel.report_type,
            period: channel.period,
            status: channel.status,
            connection_policy: channel.connection_policy,
            config,
        }))
    }

    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>> {
        info!("Converting MC device: {:?}", device);

        Ok(Arc::new(McDevice {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        info!("Converting MC point: {:?}", point);

        let raw_address = point
            .driver_config
            .get("address")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let logical = McLogicalAddress::parse(&raw_address).map_err(|e| {
            DriverError::ConfigurationError(format!("Invalid MC address '{}': {e}", raw_address))
        })?;

        let address = McAddress {
            raw: raw_address,
            logical: Some(logical),
        };

        Ok(Arc::new(McPoint {
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
            address,
            string_len_bytes: point
                .driver_config
                .get("stringLenBytes")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16),
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        info!("Converting MC action: {:?}", action);
        let inputs = action
            .inputs
            .into_iter()
            .map(|input| {
                let raw_address = input
                    .driver_config
                    .get("address")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        DriverError::ConfigurationError(format!(
                            "address is required for MC action parameter '{}'",
                            input.name
                        ))
                    })?
                    .to_string();

                let logical = McLogicalAddress::parse(&raw_address).map_err(|e| {
                    DriverError::ConfigurationError(format!(
                        "Invalid MC address '{}' for action parameter '{}': {e}",
                        raw_address, input.name
                    ))
                })?;

                let address = McAddress {
                    raw: raw_address,
                    logical: Some(logical),
                };

                let string_len_bytes = input
                    .driver_config
                    .get("stringLenBytes")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u16);

                Ok(McParameter {
                    name: input.name,
                    key: input.key,
                    data_type: input.data_type,
                    required: input.required,
                    default_value: input.default_value,
                    max_value: input.max_value,
                    min_value: input.min_value,
                    address,
                    string_len_bytes,
                })
            })
            .collect::<DriverResult<Vec<McParameter>>>()?;

        Ok(Arc::new(McAction {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters: inputs,
        }))
    }
}
