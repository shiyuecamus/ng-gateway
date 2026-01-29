//! Model converter for the MC driver.
//!
//! This module converts DB models (`ChannelModel`, `DeviceModel`, `PointModel`, `ActionModel`)
//! into typed runtime objects used by the driver implementation.
//!
//! IMPORTANT:
//! - Conversions MUST be deterministic.
//! - Conversions MUST NOT perform any network or blocking I/O.

use super::{
    protocol::frame::addr::McLogicalAddress,
    types::{McAction, McAddress, McChannel, McChannelConfig, McDevice, McParameter, McPoint},
};
use ng_gateway_sdk::{
    supervision::converter::SouthwardModelConverter, ActionModel, ChannelModel, DeviceModel,
    DriverError, DriverResult, PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice,
    RuntimePoint,
};
use std::sync::Arc;
use tracing::info;

/// Default model converter for the MC driver.
#[derive(Debug, Clone, Default)]
pub struct McConverter;

impl SouthwardModelConverter for McConverter {
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
            DriverError::ConfigurationError(format!("Invalid MC address '{raw_address}': {e}"))
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
            transform: point.transform,
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
                    .ok_or(DriverError::ConfigurationError(format!(
                        "address is required for MC action parameter '{}'",
                        input.name
                    )))?
                    .to_string();

                let logical = McLogicalAddress::parse(&raw_address).map_err(|e| {
                    DriverError::ConfigurationError(format!(
                        "Invalid MC address '{raw_address}' for action parameter '{}': {e}",
                        input.name
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
                    transform: input.transform,
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
