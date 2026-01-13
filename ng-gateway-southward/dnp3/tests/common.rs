use ng_driver_dnp3::types::{
    Dnp3Action, Dnp3Channel, Dnp3ChannelConfig, Dnp3CommandType, Dnp3Connection, Dnp3Device,
    Dnp3Parameter, Dnp3Point, Dnp3PointGroup,
};
use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, NorthwardData,
    NorthwardPublisher, ReportType, RuntimeChannel, RuntimeDevice, RuntimePoint,
    SouthwardInitContext, Status,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Once},
};
use tracing::Level;

/// Lock a `Mutex<T>` without panicking.
///
/// # Notes
/// If the mutex is poisoned, we log an error and recover the inner value to
/// keep integration tests running and observable.
fn lock_unpoisoned<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("CapturingPublisher mutex poisoned; recovering inner value");
            poisoned.into_inner()
        }
    }
}

/// A publisher that captures received data for verification.
#[derive(Debug)]
pub struct CapturingPublisher {
    pub received_data: Arc<Mutex<Vec<NorthwardData>>>,
}

impl Default for CapturingPublisher {
    fn default() -> Self {
        Self::new()
    }
}

impl CapturingPublisher {
    pub fn new() -> Self {
        Self {
            received_data: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[allow(unused)]
    pub fn get_data(&self) -> Vec<NorthwardData> {
        lock_unpoisoned(&self.received_data).clone()
    }

    #[allow(unused)]
    pub fn clear(&self) {
        lock_unpoisoned(&self.received_data).clear();
    }
}

impl NorthwardPublisher for CapturingPublisher {
    fn try_publish(&self, data: Arc<NorthwardData>) -> ng_gateway_sdk::NorthwardResult<()> {
        lock_unpoisoned(&self.received_data).push((*data).clone());
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

pub fn build_test_topology(
    host: &str,
    port: u16,
    master_addr: u16,
    outstation_addr: u16,
) -> (Dnp3Channel, Dnp3Device, Vec<Dnp3Point>) {
    let connection_policy = ConnectionPolicy::default();

    let channel_config = Dnp3ChannelConfig {
        connection: Dnp3Connection::Tcp {
            host: host.to_string(),
            port,
        },
        local_addr: master_addr,
        remote_addr: outstation_addr,
        response_timeout_ms: 5000,
        integrity_scan_interval_ms: 10000,
        event_scan_interval_ms: 1000,
    };

    let channel = Dnp3Channel {
        id: 1,
        name: "dnp3-test-channel".to_string(),
        driver_id: 1,
        collection_type: CollectionType::Report, // DNP3 is typically push/event driven or polled in background
        report_type: ReportType::Change,
        period: Some(1000),
        status: Status::Enabled,
        connection_policy,
        config: channel_config,
    };

    let device = Dnp3Device {
        id: 1,
        channel_id: channel.id,
        device_name: "dnp3-outstation".to_string(),
        device_type: "dnp3-device".to_string(),
        status: Status::Enabled,
    };

    // Create some typical points
    // - Analog Input (Group 30)
    // - Binary Input (Group 1)
    // - Octet String (Group 110) - requires the outstation to expose Group 110 points
    let points = vec![
        Dnp3Point {
            id: 1,
            device_id: device.id,
            name: "Binary Input 0".to_string(),
            key: "bi_0".to_string(),
            r#type: DataPointType::Telemetry,
            data_type: DataType::Boolean,
            access_mode: AccessMode::Read,
            unit: None,
            min_value: None,
            max_value: None,
            scale: None,
            group: Dnp3PointGroup::BinaryInput,
            index: 0,
        },
        Dnp3Point {
            id: 2,
            device_id: device.id,
            name: "Analog Input 0".to_string(),
            key: "ai_0".to_string(),
            r#type: DataPointType::Telemetry,
            data_type: DataType::Float64,
            access_mode: AccessMode::Read,
            unit: None,
            min_value: None,
            max_value: None,
            scale: None,
            group: Dnp3PointGroup::AnalogInput,
            index: 0,
        },
        Dnp3Point {
            id: 3,
            device_id: device.id,
            name: "Octet String 0".to_string(),
            key: "os_0".to_string(),
            r#type: DataPointType::Telemetry,
            // The driver currently hex-encodes raw bytes into a JSON string.
            data_type: DataType::String,
            access_mode: AccessMode::Read,
            unit: None,
            min_value: None,
            max_value: None,
            scale: None,
            group: Dnp3PointGroup::OctetString,
            index: 0,
        },
    ];

    (channel, device, points)
}

#[allow(unused)]
pub fn build_test_actions(device_id: i32) -> Vec<Dnp3Action> {
    vec![
        Dnp3Action {
            id: 1,
            device_id,
            name: "Control Relay 0".to_string(),
            command: "control_relay".to_string(),
            input_parameters: vec![Dnp3Parameter {
                name: "Value".to_string(),
                key: "value".to_string(),
                data_type: DataType::Boolean,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                group: Dnp3CommandType::CROB,
                index: 0,
                // CROB-only fields. Tests keep them empty so driver defaults are exercised.
                crob_count: None,
                crob_on_time_ms: None,
                crob_off_time_ms: None,
            }],
        },
        Dnp3Action {
            id: 2,
            device_id,
            name: "Analog Output 0".to_string(),
            command: "analog_output".to_string(),
            input_parameters: vec![Dnp3Parameter {
                name: "Value".to_string(),
                key: "value".to_string(),
                data_type: DataType::Float32,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                group: Dnp3CommandType::AnalogOutputCommand,
                index: 0,
                // CROB-only fields. Not applicable for analog outputs.
                crob_count: None,
                crob_on_time_ms: None,
                crob_off_time_ms: None,
            }],
        },
    ]
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
    channel: Dnp3Channel,
    device: Dnp3Device,
    points: Vec<Dnp3Point>,
    publisher: Arc<dyn NorthwardPublisher>,
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

    let mut points_by_device = HashMap::new();
    points_by_device.insert(device_id, runtime_points_vec);

    let ctx = SouthwardInitContext {
        devices: vec![Arc::clone(&device_arc)],
        points_by_device,
        runtime_channel,
        publisher,
    };

    (ctx, device_arc, runtime_points)
}
