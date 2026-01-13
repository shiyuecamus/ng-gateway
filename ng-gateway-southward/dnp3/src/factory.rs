use super::{
    driver::Dnp3Driver,
    types::{
        Dnp3Action, Dnp3Channel, Dnp3CommandType, Dnp3Device, Dnp3Parameter, Dnp3Point,
        Dnp3PointGroup,
    },
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    ActionModel, ChannelModel, DeviceModel, Driver, DriverError, DriverFactory, DriverResult,
    PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint, SouthwardInitContext,
};
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone, Default)]
pub struct Dnp3DriverFactory;

#[async_trait]
impl DriverFactory for Dnp3DriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        Ok(Box::new(Dnp3Driver::with_context(ctx)?))
    }

    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting DNP3 channel: {:?}", channel);

        Ok(Arc::new(Dnp3Channel {
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
                    "Failed to deserialize Dnp3ChannelConfig: {e}"
                ))
            })?,
        }))
    }

    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>> {
        // DNP3 addresses are at channel level in this design, so device config is minimal
        Ok(Arc::new(Dnp3Device {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let driver_config = point.driver_config;
        let group_val = driver_config
            .get("group")
            .ok_or(DriverError::ConfigurationError(
                "group is required for DNP3 point".to_string(),
            ))?;

        let group: Dnp3PointGroup = serde_json::from_value(group_val.clone()).map_err(|e| {
            DriverError::ConfigurationError(format!("Invalid DNP3 point group: {}", e))
        })?;

        let index = driver_config.get("index").and_then(|v| v.as_u64()).ok_or(
            DriverError::ConfigurationError("index is required for DNP3 point".to_string()),
        )? as u16;

        Ok(Arc::new(Dnp3Point {
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
            group,
            index,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        let inputs = action
            .inputs
            .into_iter()
            .map(|input| {
                let driver_config = input.driver_config;
                let group_val =
                    driver_config
                        .get("group")
                        .ok_or(DriverError::ConfigurationError(
                            "group is required for DNP3 input".to_string(),
                        ))?;

                let group: Dnp3CommandType =
                    serde_json::from_value(group_val.clone()).map_err(|e| {
                        DriverError::ConfigurationError(format!(
                            "Invalid DNP3 command group: {}",
                            e
                        ))
                    })?;

                let index = driver_config.get("index").and_then(|v| v.as_u64()).ok_or(
                    DriverError::ConfigurationError("index is required for DNP3 input".to_string()),
                )? as u16;

                // Optional CROB tuning knobs (applies only when group == CROB).
                let crob_count: Option<u8> = driver_config
                    .get("crobCount")
                    .and_then(|v| v.as_u64())
                    .map(|n| {
                        u8::try_from(n).map_err(|_| {
                            DriverError::ConfigurationError(format!(
                                "crobCount out of range (0..=255): {}",
                                n
                            ))
                        })
                    })
                    .transpose()?;

                let crob_on_time_ms: Option<u32> = driver_config
                    .get("crobOnTimeMs")
                    .and_then(|v| v.as_u64())
                    .map(|n| {
                        u32::try_from(n).map_err(|_| {
                            DriverError::ConfigurationError(format!(
                                "crobOnTimeMs out of range (0..={}): {}",
                                u32::MAX,
                                n
                            ))
                        })
                    })
                    .transpose()?;

                let crob_off_time_ms: Option<u32> = driver_config
                    .get("crobOffTimeMs")
                    .and_then(|v| v.as_u64())
                    .map(|n| {
                        u32::try_from(n).map_err(|_| {
                            DriverError::ConfigurationError(format!(
                                "crobOffTimeMs out of range (0..={}): {}",
                                u32::MAX,
                                n
                            ))
                        })
                    })
                    .transpose()?;

                Ok(Dnp3Parameter {
                    name: input.name,
                    key: input.key,
                    data_type: input.data_type,
                    required: input.required,
                    default_value: input.default_value,
                    max_value: input.max_value,
                    min_value: input.min_value,
                    group,
                    index,
                    crob_count,
                    crob_on_time_ms,
                    crob_off_time_ms,
                })
            })
            .collect::<DriverResult<Vec<Dnp3Parameter>>>()?;

        Ok(Arc::new(Dnp3Action {
            id: action.id,
            device_id: action.device_id,
            name: action.name,
            command: action.command,
            input_parameters: inputs,
        }))
    }
}
