//! Model converter for the Camera driver.
//!
//! Converts DB models (`ChannelModel`, `DeviceModel`, `PointModel`, `ActionModel`)
//! into typed runtime objects used by the camera driver implementation.
//!
//! # Constraints
//! - Conversions MUST be deterministic and side-effect free.
//! - Conversions MUST NOT perform any network or blocking I/O.

use crate::types::{
    CameraAction, CameraChannel, CameraCommand, CameraDevice, CameraOutputKey, CameraParameter,
    CameraPoint,
};
use ng_gateway_sdk::{
    supervision::converter::SouthwardModelConverter, ActionModel, ChannelModel, DeviceModel,
    DriverError, DriverResult, PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice,
    RuntimePoint,
};
use std::sync::Arc;
use tracing::info;

/// Default model converter for the Camera driver.
#[derive(Debug, Clone, Default)]
pub struct CameraModelConverter;

impl SouthwardModelConverter for CameraModelConverter {
    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!(channel_id = channel.id, "Converting Camera channel");
        Ok(Arc::new(CameraChannel {
            id: channel.id,
            name: channel.name,
            driver_id: channel.driver_id,
            collection_type: channel.collection_type,
            report_type: channel.report_type,
            period: channel.period,
            status: channel.status,
            connection_policy: channel.connection_policy,
            config: serde_json::from_value(channel.driver_config).map_err(|e| {
                DriverError::ConfigurationError(format!(
                    "Failed to deserialize CameraChannelConfig: {e}"
                ))
            })?,
        }))
    }

    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>> {
        Ok(Arc::new(CameraDevice {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let output_key_str = point
            .driver_config
            .get("outputKey")
            .and_then(|v| v.as_str())
            .ok_or(DriverError::ConfigurationError(
                "Camera point requires 'outputKey' in driver_config".to_string(),
            ))?;
        let output_key = CameraOutputKey::try_from(output_key_str)?;
        let custom_expression = point
            .driver_config
            .get("customExpression")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(Arc::new(CameraPoint {
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
            output_key,
            custom_expression,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let command = CameraCommand::try_from(action.command.as_str())?;

        let input_parameters = action
            .inputs
            .into_iter()
            .map(|p| CameraParameter {
                name: p.name,
                key: p.key,
                data_type: p.data_type,
                required: p.required,
                default_value: p.default_value,
                max_value: p.max_value,
                min_value: p.min_value,
                transform: p.transform,
            })
            .collect();

        Ok(Arc::new(CameraAction {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command,
            input_parameters,
        }))
    }
}
