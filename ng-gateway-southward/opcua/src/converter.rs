//! Model converter for the OPC UA driver.
//!
//! Converts DB models (`ChannelModel`, `DeviceModel`, `PointModel`, `ActionModel`) into typed
//! runtime objects used by the driver implementation.
//!
//! IMPORTANT:
//! - Conversions MUST be deterministic.
//! - Conversions MUST NOT perform any network or blocking I/O.

use super::types::{OpcUaAction, OpcUaChannel, OpcUaDevice, OpcUaParameter, OpcUaPoint};
use ng_gateway_sdk::{
    supervision::converter::SouthwardModelConverter, ActionModel, ChannelModel, DeviceModel,
    DriverError, DriverResult, PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice,
    RuntimePoint,
};
use std::sync::Arc;
use tracing::info;

/// Default model converter for the OPC UA driver.
#[derive(Debug, Clone, Default)]
pub struct OpcUaConverter;

impl SouthwardModelConverter for OpcUaConverter {
    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting OPC UA channel: {:?}", channel);
        Ok(Arc::new(OpcUaChannel {
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
        Ok(Arc::new(OpcUaDevice {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let driver_config = point.driver_config;
        let node_id = driver_config
            .get("nodeId")
            .and_then(|v| v.as_str())
            .ok_or(DriverError::ConfigurationError(
                "nodeId is required for OPC UA point".to_string(),
            ))?
            .to_string();

        Ok(Arc::new(OpcUaPoint {
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
            node_id,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let inputs = action
            .inputs
            .into_iter()
            .map(|input| {
                let driver_config = input.driver_config;
                let node_id = driver_config
                    .get("nodeId")
                    .and_then(|v| v.as_str())
                    .ok_or(DriverError::ConfigurationError(
                        "nodeId is required for OPC UA input".to_string(),
                    ))?
                    .to_string();
                Ok(OpcUaParameter {
                    name: input.name,
                    key: input.key,
                    data_type: input.data_type,
                    required: input.required,
                    default_value: input.default_value,
                    max_value: input.max_value,
                    min_value: input.min_value,
                    transform: input.transform,
                    node_id,
                })
            })
            .collect::<DriverResult<Vec<OpcUaParameter>>>()?;

        Ok(Arc::new(OpcUaAction {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters: inputs,
        }))
    }
}
