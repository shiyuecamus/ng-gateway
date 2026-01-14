use ng_driver_modbus::types::{
    Endianness, ModbusChannel, ModbusChannelConfig, ModbusConnection, ModbusDevice,
    ModbusFunctionCode, ModbusPoint,
};
use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, NorthwardData,
    NorthwardPublisher, ReportType, RuntimeChannel, RuntimeDevice, RuntimePoint,
    SouthwardInitContext, Status,
};
use std::{
    collections::HashMap,
    sync::{Arc, Once},
};
use tracing::Level;

// ============================================================================
// 用户配置区域 (USER CONFIGURATION AREA)
// ============================================================================

pub const SLAVE_IP: &str = "8.155.153.52"; // 模拟器/设备 IP
pub const SLAVE_PORT: u16 = 502; // Modbus TCP 端口
pub const SLAVE_ID: u8 = 1; // 从站地址 (Unit ID)

/// Simple no-op northward publisher used only for testing.
#[derive(Debug)]
pub struct TestPublisher;

impl NorthwardPublisher for TestPublisher {
    fn try_publish(&self, data: Arc<NorthwardData>) -> ng_gateway_sdk::NorthwardResult<()> {
        // In a real test, you might want to send this to a channel to verify assertions.
        // For now, we just log it.
        match data.as_ref() {
            NorthwardData::Telemetry(t) => {
                tracing::info!(target: "modbus::test", "Published Telemetry: {:?}", t.values);
            }
            NorthwardData::Attributes(a) => {
                tracing::info!(target: "modbus::test", "Published Attributes (Client): {:?}", a.client_attributes);
                tracing::info!(target: "modbus::test", "Published Attributes (Shared): {:?}", a.shared_attributes);
                tracing::info!(target: "modbus::test", "Published Attributes (Server): {:?}", a.server_attributes);
            }
            _ => {}
        }
        Ok(())
    }
}

static INIT_TRACING: Once = Once::new();

pub fn init_tracing() {
    INIT_TRACING.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .with_target(false)
            .without_time()
            .try_init();
    });
}

pub struct TestPointConfig {
    pub name: String,
    pub address: u16,
    pub quantity: u16,
    pub function_code: ModbusFunctionCode,
    pub data_type: DataType,
    pub access_mode: AccessMode,
}

/// Build a Modbus TCP channel, device and points for integration testing.
pub fn build_modbus_tcp_topology(
    host: &str,
    port: u16,
    slave_id: u8,
    point_configs: Vec<TestPointConfig>,
) -> (ModbusChannel, ModbusDevice, Vec<ModbusPoint>) {
    let connection_policy = ConnectionPolicy {
        read_timeout_ms: 2000,
        write_timeout_ms: 2000,
        ..Default::default()
    };

    let channel_config = ModbusChannelConfig {
        connection: ModbusConnection::Tcp {
            host: host.to_string(),
            port,
        },
        byte_order: Endianness::BigEndian, // Standard Modbus is Big Endian
        word_order: Endianness::BigEndian,
        max_gap: 10,
        max_batch: 100,
    };

    let channel = ModbusChannel {
        id: 1,
        name: "test-modbus-tcp".to_string(),
        driver_id: 1,
        collection_type: CollectionType::Collection,
        report_type: ReportType::Change,
        period: Some(1000),
        status: Status::Enabled,
        connection_policy,
        config: channel_config,
    };

    let device = ModbusDevice {
        id: 1,
        channel_id: channel.id,
        device_name: "test-device".to_string(),
        device_type: "modbus-tcp-slave".to_string(),
        status: Status::Enabled,
        slave_id,
    };

    let points: Vec<ModbusPoint> = point_configs
        .into_iter()
        .enumerate()
        .map(|(idx, cfg)| ModbusPoint {
            id: (idx + 1) as i32,
            device_id: device.id,
            name: cfg.name.clone(),
            key: cfg.name,
            r#type: DataPointType::Telemetry,
            data_type: cfg.data_type,
            access_mode: cfg.access_mode,
            unit: None,
            min_value: None,
            max_value: None,
            scale: None,
            function_code: cfg.function_code,
            address: cfg.address,
            quantity: cfg.quantity,
        })
        .collect();

    (channel, device, points)
}

pub type InitContextBundle = (
    SouthwardInitContext,
    Arc<dyn RuntimeDevice>,
    Arc<[Arc<dyn RuntimePoint>]>,
);

pub fn build_init_context(
    channel: ModbusChannel,
    device: ModbusDevice,
    points: Vec<ModbusPoint>,
) -> InitContextBundle {
    let device_id = device.id;

    let runtime_channel: Arc<dyn RuntimeChannel> = Arc::new(channel);
    let device_arc: Arc<dyn RuntimeDevice> = Arc::new(device);
    let runtime_points_vec: Vec<Arc<dyn RuntimePoint>> = points
        .into_iter()
        .map(|p| Arc::new(p) as Arc<dyn RuntimePoint>)
        .collect();
    let runtime_points: Arc<[Arc<dyn RuntimePoint>]> = Arc::from(runtime_points_vec.as_slice());

    let mut points_by_device: HashMap<i32, Vec<Arc<dyn RuntimePoint>>> = HashMap::new();
    points_by_device.insert(device_id, runtime_points_vec);

    let ctx = SouthwardInitContext {
        devices: vec![Arc::clone(&device_arc)],
        points_by_device,
        runtime_channel,
        publisher: Arc::new(TestPublisher),
    };

    (ctx, device_arc, runtime_points)
}
