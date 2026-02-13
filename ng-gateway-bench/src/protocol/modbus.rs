use crate::protocol::{ChannelRuntime, PointsByDevice};
use crate::publisher::NullPublisher;
use ng_driver_modbus::{
    types::{
        Endianness, ModbusChannel, ModbusChannelConfig, ModbusConnection, ModbusDevice,
        ModbusFunctionCode, ModbusPoint,
    },
    ModbusConnector,
};
use ng_gateway_sdk::{
    supervision::{Connector, NoopObserverFactory, SupervisorLoop, SupervisorParams},
    AccessMode, CollectionType, CollectorConcurrencyProfile, ConnectionPolicy, DataPointType,
    DataType, Driver, DriverResult, NoopSouthwardTransportMeter, ReportType, RuntimeChannel,
    RuntimeDevice, RuntimePoint, SouthwardInitContext, Status, SupervisedDriver, Transform,
};
use std::{collections::HashMap, sync::Arc};

/// Arguments for `build_modbus_channel_runtime`.
///
/// # Design
/// This struct exists to keep the benchmark harness readable and to satisfy
/// `clippy::too_many_arguments` when clippy is run with `-D warnings`.
#[derive(Debug, Clone)]
pub struct ModbusChannelRuntimeArgs {
    /// Channel index in the benchmark scenario (0-based).
    pub channel_idx: usize,
    /// Device count per channel.
    pub devices_per_channel: usize,
    /// Point count per device.
    pub points_per_device: usize,
    /// Collection period in milliseconds.
    pub period_ms: u32,
    /// Modbus TCP host.
    pub host: String,
    /// Modbus TCP port.
    pub port: u16,
    /// Base slave id (unit id).
    ///
    /// This is the start of the slave id range. For typical simulators you may set:
    /// - base = 1
    /// - max  = 10
    pub slave_id_base: u8,
    /// Maximum slave id (unit id).
    ///
    /// Devices will be assigned a slave id in `[slave_id_base, slave_id_max]` in a round-robin
    /// fashion using a global device index across channels.
    pub slave_id_max: u8,
    /// Base register address.
    pub address_base: u16,
    /// Address step per point (register units).
    pub address_step: u16,
    /// TCP pool size per channel.
    pub tcp_pool_size: u16,
}

/// Create a Modbus channel runtime for benchmarks.
///
/// # Assumptions
/// - `points_per_device` points are mapped to Modbus holding registers.
/// - Float32 points use `quantity = 2` registers.
/// - Address pattern is `base + i * address_step`.
pub fn build_modbus_channel_runtime(
    args: ModbusChannelRuntimeArgs,
) -> DriverResult<ChannelRuntime> {
    let runtime_channel: Arc<ModbusChannel> = Arc::new(ModbusChannel {
        id: (1000 + args.channel_idx) as i32,
        name: format!("bench-modbus-ch-{}", args.channel_idx),
        driver_id: 0,
        collection_type: CollectionType::Collection,
        report_type: ReportType::Always,
        period: Some(args.period_ms),
        status: Status::Enabled,
        connection_policy: ConnectionPolicy::default(),
        config: ModbusChannelConfig {
            connection: ModbusConnection::Tcp {
                host: args.host,
                port: args.port,
            },
            byte_order: Endianness::BigEndian,
            word_order: Endianness::BigEndian,
            max_batch_registers: 120,
            max_gap_registers: 1,
            max_batch_bits: 2000,
            max_gap_bits: 500,
            tcp_pool_size: args.tcp_pool_size,
            max_timeouts: 3,
        },
    });

    let publisher = Arc::new(NullPublisher::default());

    let mut devices: Vec<Arc<dyn RuntimeDevice>> = Vec::with_capacity(args.devices_per_channel);
    let mut points_by_device: HashMap<i32, Vec<Arc<dyn RuntimePoint>>> =
        HashMap::with_capacity(args.devices_per_channel);

    // Build devices and points.
    for dev_idx in 0..args.devices_per_channel {
        // Assign slave id per device (global across channels).
        let slave_id = assign_modbus_slave_id(
            args.channel_idx,
            args.devices_per_channel,
            dev_idx,
            args.slave_id_base,
            args.slave_id_max,
        );

        // Keep ids within i32 range even for high scenario fan-out.
        let dev_id = (1_000_000i32)
            .saturating_add((args.channel_idx as i32).saturating_mul(10_000))
            .saturating_add(dev_idx as i32);
        let dev = Arc::new(ModbusDevice {
            id: dev_id,
            channel_id: runtime_channel.id,
            device_name: format!("bench-modbus-dev-{}-{}", args.channel_idx, dev_idx),
            device_type: "bench-modbus".to_string(),
            status: Status::Enabled,
            slave_id,
        }) as Arc<dyn RuntimeDevice>;

        let mut pts: Vec<Arc<dyn RuntimePoint>> = Vec::with_capacity(args.points_per_device);
        for i in 0..args.points_per_device {
            let addr = args
                .address_base
                .saturating_add((i as u16).saturating_mul(args.address_step));
            let point = ModbusPoint {
                id: (2_000_000i32)
                    .saturating_add((args.channel_idx as i32).saturating_mul(1_000_000))
                    .saturating_add((dev_idx as i32).saturating_mul(10_000))
                    .saturating_add(i as i32),
                device_id: dev_id,
                name: format!("p{}", i),
                key: format!("dev{}_p{}", dev_idx, i),
                r#type: DataPointType::Telemetry,
                data_type: DataType::Float32,
                access_mode: AccessMode::ReadWrite,
                unit: None,
                min_value: None,
                max_value: None,
                transform: Transform::default(),
                function_code: ModbusFunctionCode::ReadHoldingRegisters,
                address: addr,
                // Float32 consumes 2 registers.
                quantity: 2,
            };
            pts.push(Arc::new(point) as Arc<dyn RuntimePoint>);
        }

        points_by_device.insert(dev_id, pts);
        devices.push(dev);
    }

    // Convert points_by_device into driver expected shape.
    let mut collect_items: PointsByDevice = Vec::with_capacity(devices.len());
    let mut downlink_points: Vec<Arc<dyn RuntimePoint>> = Vec::new();

    for dev in devices.iter() {
        let dev_id = dev.id();
        let pts = points_by_device.get(&dev_id).cloned().unwrap_or_default();
        let pts_arc: Arc<[Arc<dyn RuntimePoint>]> = Arc::from(pts.into_boxed_slice());

        if downlink_points.is_empty() {
            // Store a cheap subset for write tests (first device only).
            downlink_points = pts_arc.iter().take(200).cloned().collect();
        }

        collect_items.push((Arc::clone(dev), pts_arc));
    }

    let channel_id = runtime_channel.id();

    let ctx = SouthwardInitContext {
        devices: devices.clone(),
        points_by_device: collect_items
            .iter()
            .map(|(d, pts)| (d.id(), pts.to_vec()))
            .collect(),
        runtime_channel: runtime_channel as Arc<dyn RuntimeChannel>,
        publisher,
        channel_id,
        transport_meter: Arc::new(NoopSouthwardTransportMeter),
        observer_factory: Arc::new(NoopObserverFactory),
    };

    let connector = <ModbusConnector as Connector>::new(ctx)?;
    let (loop_, _state_rx) = SupervisorLoop::new_noop_with_span(
        connector,
        SupervisorParams::default(),
        tracing::Span::current(),
    );
    let driver: SupervisedDriver<ModbusConnector> = SupervisedDriver::new_with_concurrency_profile(
        loop_,
        CollectorConcurrencyProfile::from_io_lanes(args.tcp_pool_size as usize),
    );
    let driver: Arc<dyn Driver> = Arc::new(driver);

    Ok(ChannelRuntime {
        channel_idx: args.channel_idx,
        driver,
        devices,
        points_by_device: collect_items,
        downlink_points,
    })
}

/// Assign a Modbus slave id for a device in a benchmark topology.
///
/// # Behavior
/// - Builds a global device index across channels: `channel_idx * devices_per_channel + dev_idx`.
/// - Maps that index into a configured slave id range: `[base, max]` (inclusive).
/// - Clamps the final value to `[1, 247]` (driver validation range).
fn assign_modbus_slave_id(
    channel_idx: usize,
    devices_per_channel: usize,
    dev_idx: usize,
    base: u8,
    max: u8,
) -> u8 {
    let base_u16 = u16::from(base).clamp(1, 247);
    let max_u16 = u16::from(max).clamp(1, 247);
    let max_u16 = max_u16.max(base_u16);

    let range = max_u16 - base_u16 + 1;
    let global_index_1based: u16 = (channel_idx as u16)
        .saturating_mul(devices_per_channel as u16)
        .saturating_add(dev_idx as u16)
        .saturating_add(1);

    let offset = (global_index_1based - 1) % range;
    let slave_id = base_u16 + offset;
    slave_id as u8
}
