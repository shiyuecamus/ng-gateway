//! Protocol adapters for benchmark harness.
//!
//! This module provides a unified way to create driver instances, build runtime
//! device/point topologies, and run collect/write operations across different
//! southward protocols (e.g. Modbus, OPC UA).

pub mod modbus;
pub mod opcua;

use ng_gateway_sdk::{
    Driver, DriverResult, NGValue, NorthwardData, RuntimeDevice, RuntimePoint, WriteResult,
};
use std::{sync::Arc, time::Duration};

/// A pre-built grouping item for `collect_data`: (device, points_of_device).
///
/// # Rationale
/// This type alias keeps signatures readable and avoids `clippy::type_complexity`
/// in the benchmark harness code.
pub type PointsByDeviceItem = (Arc<dyn RuntimeDevice>, Arc<[Arc<dyn RuntimePoint>]>);

/// Points grouped by device for efficient `collect_data` calls.
pub type PointsByDevice = Vec<PointsByDeviceItem>;

/// A single channel runtime unit under test: driver + runtime objects.
pub struct ChannelRuntime {
    /// Channel index in the benchmark scenario (0-based).
    pub channel_idx: usize,
    /// The driver instance for this channel.
    pub driver: Arc<dyn Driver>,
    /// Business devices attached to this channel.
    pub devices: Vec<Arc<dyn RuntimeDevice>>,
    /// Points grouped by device id, pre-built as Arc slices for collect calls.
    pub points_by_device: PointsByDevice,
    /// Points for downlink tests (subset, typically first N points of the first device).
    pub downlink_points: Vec<Arc<dyn RuntimePoint>>,
}

impl std::fmt::Debug for ChannelRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelRuntime")
            .field("channel_idx", &self.channel_idx)
            .field("devices", &self.devices.len())
            .field("points_by_device", &self.points_by_device.len())
            .field("downlink_points", &self.downlink_points.len())
            .finish()
    }
}

impl ChannelRuntime {
    /// Start the driver.
    pub async fn start(&self) -> DriverResult<()> {
        self.driver.start().await
    }

    /// Stop the driver.
    pub async fn stop(&self) -> DriverResult<()> {
        self.driver.stop().await
    }

    /// Perform one collection cycle for this channel.
    pub async fn collect_once(&self) -> DriverResult<Vec<NorthwardData>> {
        self.driver.collect_data(&self.points_by_device).await
    }

    /// Write one point (control-plane).
    pub async fn write_point_once(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: NGValue,
        timeout: Option<Duration>,
    ) -> DriverResult<WriteResult> {
        let timeout_ms = timeout.map(|d| d.as_millis() as u64);
        self.driver
            .write_point(device, point, value, timeout_ms)
            .await
    }
}
