//! Model converter for the Ethernet/IP driver.
//!
//! Converts DB models (`ChannelModel`, `DeviceModel`, `PointModel`, `ActionModel`) into typed
//! runtime objects used by the driver implementation.
//!
//! IMPORTANT:
//! - Conversions MUST be deterministic.
//! - Conversions MUST NOT perform any network or blocking I/O.
//! - No legacy fallback behavior is allowed (strict config requirements).

use super::types::{
    EthernetIpAction, EthernetIpChannel, EthernetIpDevice, EthernetIpParameter, EthernetIpPoint,
};
use ng_gateway_sdk::{
    supervision::converter::SouthwardModelConverter, ActionModel, ChannelModel, DeviceModel,
    DriverError, DriverResult, PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice,
    RuntimePoint,
};
use std::sync::Arc;
use tracing::info;

/// Default model converter for the Ethernet/IP driver.
#[derive(Debug, Clone, Copy, Default)]
pub struct EthernetIpConverter;

impl SouthwardModelConverter for EthernetIpConverter {
    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting Ethernet/IP channel: {:?}", channel);
        Ok(Arc::new(EthernetIpChannel {
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
                    "Failed to deserialize EthernetIpChannelConfig: {e}"
                ))
            })?,
        }))
    }

    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>> {
        Ok(Arc::new(EthernetIpDevice {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let driver_config = point.driver_config;
        let tag_name = driver_config
            .get("tagName")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or(DriverError::ConfigurationError(
                "tagName is required and must be a non-empty string".into(),
            ))?
            .to_string();

        Ok(Arc::new(EthernetIpPoint {
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
            tag_name,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let inputs = action
            .inputs
            .into_iter()
            .map(|input| -> DriverResult<EthernetIpParameter> {
                let tag_name = input
                    .driver_config
                    .get("tagName")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .ok_or(DriverError::ConfigurationError(format!(
                        "Ethernet/IP action input '{}' missing required driver_config.tagName",
                        input.key
                    )))?
                    .to_string();

                Ok(EthernetIpParameter {
                    name: input.name,
                    key: input.key,
                    data_type: input.data_type,
                    required: input.required,
                    default_value: input.default_value,
                    max_value: input.max_value,
                    min_value: input.min_value,
                    transform: input.transform,
                    tag_name,
                })
            })
            .collect::<DriverResult<Vec<_>>>()?;

        Ok(Arc::new(EthernetIpAction {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters: inputs,
        }))
    }
}
