//! Model converter for the S7 driver.
//!
//! This module converts DB models (`ChannelModel`, `DeviceModel`, `PointModel`, `ActionModel`)
//! into typed runtime objects used by the driver implementation.
//!
//! IMPORTANT:
//! - Conversions MUST be deterministic.
//! - Conversions MUST NOT perform any network or blocking I/O.

use super::{
    protocol::frame::parse_s7_address,
    types::{S7Action, S7Channel, S7Device, S7Parameter, S7Point},
};
use ng_gateway_sdk::{
    supervision::converter::SouthwardModelConverter, ActionModel, ChannelModel, DeviceModel,
    DriverError, DriverResult, PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice,
    RuntimePoint,
};
use std::sync::Arc;
use tracing::info;

/// Default model converter for the S7 driver.
#[derive(Debug, Clone, Default)]
pub struct S7Converter;

impl SouthwardModelConverter for S7Converter {
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
                    "Failed to deserialize S7ChannelConfig: {e}"
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
            transform: point.transform,
            address: point
                .driver_config
                .get("address")
                .and_then(|v| v.as_str())
                .map(parse_s7_address)
                .ok_or(DriverError::ConfigurationError(
                    "address is required for S7 point".to_string(),
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
                    transform: input.transform,
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
