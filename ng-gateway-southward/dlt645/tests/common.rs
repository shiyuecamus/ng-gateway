use ng_driver_dlt645::{
    protocol::frame::{encode_address_from_str, parse_di_str},
    Dl645Channel, Dl645ChannelConfig, Dl645Connection, Dl645Device, Dl645Point, Dl645Version,
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
///
/// This implementation discards all published data but records that the
/// driver was able to call into the northbound path without blocking.
#[derive(Debug)]
pub struct TestPublisher;

impl NorthwardPublisher for TestPublisher {
    fn try_publish(&self, _data: Arc<NorthwardData>) -> ng_gateway_sdk::NorthwardResult<()> {
        Ok(())
    }
}

/// Global one-time tracing initialization guard for DL/T 645 integration tests.
///
/// The `Once` ensures that `tracing_subscriber::fmt` is only installed once,
/// even if multiple tests from this crate call `init_tracing` concurrently.
static INIT_TRACING: Once = Once::new();

/// Initialize structured `tracing` subscriber for DL/T 645 tests.
///
/// The configuration:
/// - Uses `DEBUG` as the maximum log level so that reconnect paths are visible.
/// - Disables targets and timestamps to keep output compact in test runs.
pub fn init_tracing() {
    INIT_TRACING.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG)
            .with_target(false)
            .without_time()
            .try_init();
    });
}

/// Build a minimal DL/T 645 channel, device and points for integration testing.
///
/// The channel uses a TCP connection pointing to the provided host/port and
/// a small frame size suitable for unit tests and manual integration runs.
///
/// Parameters:
/// - `version`: protocol version (1997/2007) to be used by the driver.
/// - `host`: IP or hostname of the DL/T 645 slave simulator.
/// - `port`: TCP port of the DL/T 645 slave simulator.
/// - `device_address`: 6-byte BCD meter address as string.
/// - `point_dis`: list of DI strings for which points will be created.
///
/// Returns the channel, device and a vector of points matching the given DIs.
pub fn build_test_topology_for_version(
    version: Dl645Version,
    host: &str,
    port: u16,
    device_address: &str,
    point_dis: &[&str],
) -> (Dl645Channel, Dl645Device, Vec<Dl645Point>) {
    let connection_policy = ConnectionPolicy::default();

    let channel_config = Dl645ChannelConfig {
        version,
        connection: Dl645Connection::Tcp {
            host: host.to_string(),
            port,
        },
        // Preamble is disabled for tests; enable it locally if a specific
        // meter requires a wakeup sequence.
        wakeup_preamble: vec![0xFE, 0xFE, 0xFE, 0xFE],
        // Use a low threshold in tests so that consecutive timeouts are
        // detected quickly and reconnect behavior becomes visible during
        // short manual runs.
        max_timeouts: 1,
    };

    let channel = Dl645Channel {
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

    let device = Dl645Device {
        id: 1,
        channel_id: channel.id,
        device_name: "test-meter".to_string(),
        device_type: "dlt645-simulator".to_string(),
        status: Status::Enabled,
        address: encode_address_from_str(device_address).unwrap(),
        password: "00000000".to_string(),
        operator_code: None,
    };

    // Build one point per DI, assigning sequential IDs and using simple
    // keys/names derived from the index. This is sufficient for integration
    // testing and keeps the topology builder generic.
    let points: Vec<Dl645Point> = point_dis
        .iter()
        .enumerate()
        .map(|(idx, di)| {
            let di_parsed = match parse_di_str(di) {
                Some(v) => v,
                None => {
                    panic!("invalid DI in test topology, expected 4 or 8 hex chars: {di}");
                }
            };
            Dl645Point {
                id: (idx + 1) as i32,
                device_id: device.id,
                name: format!("point_{idx}"),
                key: format!("point_{idx}"),
                r#type: DataPointType::Telemetry,
                data_type: DataType::Float64,
                access_mode: AccessMode::Read,
                unit: Some("kWh".to_string()),
                min_value: None,
                max_value: None,
                scale: None,
                di: di_parsed,
                decimals: Some(2),
            }
        })
        .collect();

    (channel, device, points)
}

/// Build a `SouthwardInitContext` and runtime device/points from the test topology.
///
/// This helper transforms strongly typed DL/T 645 models into the trait
/// objects expected by `Dl645Driver::with_context` so that test cases can
/// focus on driver behavior instead of boilerplate wiring.
///
/// Clippy considers deeply nested tuples of trait objects in signatures as
/// overly complex; we keep the structure but factor it into a type alias.
pub type InitContextBundle = (
    SouthwardInitContext,
    Arc<dyn RuntimeDevice>,
    Arc<[Arc<dyn RuntimePoint>]>,
);

pub fn build_init_context(
    channel: Dl645Channel,
    device: Dl645Device,
    points: Vec<Dl645Point>,
) -> InitContextBundle {
    let device_id = device.id;

    // Move owned test models into trait objects; avoid cloning the entire models.
    let runtime_channel: Arc<dyn RuntimeChannel> = Arc::new(channel);
    let device_arc: Arc<dyn RuntimeDevice> = Arc::new(device);
    let runtime_points_vec: Vec<Arc<dyn RuntimePoint>> = points
        .into_iter()
        .map(|p| Arc::new(p) as Arc<dyn RuntimePoint>)
        .collect();
    // Keep the Vec for the context map; create the slice by cloning only the Arcs (cheap).
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
