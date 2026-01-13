use ng_driver_cjt188::types::{
    Cjt188Channel, Cjt188ChannelConfig, Cjt188Connection, Cjt188Device, Cjt188Point, Cjt188Version,
    MeterType,
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

/// Simple no-op northward publisher used only for testing.
#[derive(Debug)]
pub struct TestPublisher;

impl NorthwardPublisher for TestPublisher {
    fn try_publish(&self, _data: Arc<NorthwardData>) -> ng_gateway_sdk::NorthwardResult<()> {
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

/// Build a minimal CJ/T 188 channel, device and points for integration testing.
pub fn build_test_topology_for_version(
    version: Cjt188Version,
    host: &str,
    port: u16,
    device_address: &str,
    point_dis: &[&str],
) -> (Cjt188Channel, Cjt188Device, Vec<Cjt188Point>) {
    let connection_policy = ConnectionPolicy::default();

    let channel_config = Cjt188ChannelConfig {
        version,
        connection: Cjt188Connection::Tcp {
            host: host.to_string(),
            port,
        },
        wakeup_preamble: vec![0xFE, 0xFE, 0xFE, 0xFE],
        max_timeouts: 1,
    };

    let channel = Cjt188Channel {
        id: 1,
        name: "test-channel".to_string(),
        driver_id: 1,
        collection_type: CollectionType::Collection,
        report_type: ReportType::Change,
        period: Some(1000),
        status: Status::Enabled,
        connection_policy,
        config: channel_config,
    };

    let device = Cjt188Device {
        id: 1,
        channel_id: channel.id,
        device_name: "test-meter".to_string(),
        device_type: "cjt188-simulator".to_string(),
        status: Status::Enabled,
        meter_type: MeterType::COLD_WATER,
        address: device_address.to_string(),
    };

    let points: Vec<Cjt188Point> = point_dis
        .iter()
        .enumerate()
        .map(|(idx, di_str)| {
            let di_val = u16::from_str_radix(di_str, 16).unwrap_or_else(|_| {
                panic!("invalid DI in test topology, expected hex chars: {di_str}");
            });

            Cjt188Point {
                id: (idx + 1) as i32,
                device_id: device.id,
                name: format!("point_{idx}"),
                key: format!("point_{idx}"),
                r#type: DataPointType::Telemetry,
                data_type: DataType::Float64,
                access_mode: AccessMode::Read,
                unit: Some("m3".to_string()),
                min_value: None,
                max_value: None,
                scale: None,
                di: di_val,
                field_key: "current_flow".to_string(), // Default field key for testing
            }
        })
        .collect();

    (channel, device, points)
}

/// Return bundle for `build_init_context`.
///
/// Clippy considers deeply nested tuples of trait objects in signatures as
/// overly complex; we keep the structure but factor it into a type alias.
pub type InitContextBundle = (
    SouthwardInitContext,
    Arc<dyn RuntimeDevice>,
    Arc<[Arc<dyn RuntimePoint>]>,
);

pub fn build_init_context(
    channel: Cjt188Channel,
    device: Cjt188Device,
    points: Vec<Cjt188Point>,
) -> InitContextBundle {
    let device_id = device.id;

    // Move owned test models into trait objects; avoid cloning the entire models.
    let runtime_channel: Arc<dyn RuntimeChannel> = Arc::new(channel);
    let device_arc: Arc<dyn RuntimeDevice> = Arc::new(device);
    let runtime_points_vec: Vec<Arc<dyn RuntimePoint>> = points
        .into_iter()
        .map(|p| Arc::new(p) as Arc<dyn RuntimePoint>)
        .collect();
    // `Arc<[T]>` is used by call sites for convenient slice sharing; we keep the Vec for the
    // context map and create the slice by cloning only the Arcs (cheap refcount bumps).
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
