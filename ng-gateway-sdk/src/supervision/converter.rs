//! Low-frequency model conversion contracts.
//!
//! These traits define how database/API models are converted into runtime trait objects
//! used by the gateway core and drivers/plugins.
//!
//! # Constraints (MUST)
//! - MUST be deterministic and side-effect free.
//! - MUST NOT perform any network/blocking I/O.
//! - MUST validate input models and return actionable errors.

use crate::{
    northward::PluginConfig,
    southward::{
        model::{ActionModel, ChannelModel, DeviceModel, PointModel},
        RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint,
    },
    DriverResult, NorthwardResult,
};
use std::sync::Arc;

/// Convert southward models into runtime trait objects.
pub trait SouthwardModelConverter: Send + Sync + 'static {
    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>>;
    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>>;
    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>>;
    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>>;
}

/// Convert northward plugin config JSON into a typed config object.
pub trait NorthwardModelConverter: Send + Sync + 'static {
    fn convert_plugin_config(
        &self,
        config: serde_json::Value,
    ) -> NorthwardResult<Arc<dyn PluginConfig>>;
}
