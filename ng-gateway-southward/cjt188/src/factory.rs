use crate::{
    driver::Cjt188Driver,
    types::{Cjt188Channel, Cjt188Device, Cjt188Point, MeterType},
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    ActionModel, ChannelModel, DeviceModel, Driver, DriverError, DriverFactory, DriverResult,
    PointModel, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint, SouthwardInitContext,
};
use std::sync::Arc;
use tracing::info;

/// Factory for CJ/T 188 drivers.
///
/// The factory is responsible for constructing driver instances and converting
/// persisted models into runtime structures.
#[derive(Debug, Clone, Default)]
pub struct Cjt188DriverFactory;

#[async_trait]
impl DriverFactory for Cjt188DriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        Ok(Box::new(Cjt188Driver::with_context(ctx)?))
    }

    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        info!("Converting CJ/T 188 channel: {:?}", channel);

        Ok(Arc::new(Cjt188Channel {
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
                    "Failed to deserialize Cjt188ChannelConfig: {e}"
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
                "Device address is required for CJ/T 188 device".to_string(),
            ))?
            .to_string();

        let meter_type_code = driver_config
            .get("meterType")
            .or_else(|| driver_config.get("meter_type"))
            .and_then(|v| v.as_u64())
            .and_then(|v| u8::try_from(v).ok())
            .ok_or(DriverError::ConfigurationError(
                "Device meterType is required for CJ/T 188 device (0..=255 integer)".to_string(),
            ))?;
        let meter_type = MeterType::from(meter_type_code);

        // Validate address format (14 hex chars) early if possible,
        // but RuntimeDevice struct keeps it as string usually, parsed in driver.
        // Here we just pass it through.

        Ok(Arc::new(Cjt188Device {
            id: device.id,
            channel_id: device.channel_id,
            device_name: device.device_name,
            device_type: device.device_type,
            status: device.status,
            meter_type,
            address,
        }))
    }

    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>> {
        let driver_config = point.driver_config;
        let di_str = driver_config.get("di").and_then(|v| v.as_str()).ok_or(
            DriverError::ConfigurationError("DI is required for CJ/T 188 point".to_string()),
        )?;

        let di = u16::from_str_radix(di_str, 16).map_err(|e| {
            DriverError::ConfigurationError(format!("Invalid DI hex string '{}': {}", di_str, e))
        })?;

        // Extract field_key (REQUIRED for schema-driven parsing)
        let field_key = driver_config
            .get("field_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                DriverError::ConfigurationError(format!(
                    "Point {} (id={}) missing required field 'field_key' in driver_config",
                    point.name, point.id
                ))
            })?
            .to_string();

        Ok(Arc::new(Cjt188Point {
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
            field_key,
        }))
    }

    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>> {
        Err(DriverError::ConfigurationError(format!(
            "CJ/T 188 driver does not support actions; action '{}' (id={}) is not allowed",
            action.name, action.id
        )))
    }
}
