use ng_driver_ethernet_ip::types::{
    EthernetIpChannel, EthernetIpChannelConfig, EthernetIpDevice, EthernetIpPoint,
};
use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, NorthwardData,
    NorthwardPublisher, ReportType, RuntimeChannel, RuntimeDevice, RuntimePoint,
    SouthwardInitContext, Status,
};
use std::{
    collections::HashMap,
    sync::{Arc, Once},
    time::Duration,
};
use tracing::Level;

/// Simple no-op northward publisher used only for testing.
///
/// This implementation discards all published data but ensures that the driver
/// can be wired with a valid publisher without blocking.
#[derive(Debug)]
pub struct TestPublisher;

impl NorthwardPublisher for TestPublisher {
    fn try_publish(&self, _data: Arc<NorthwardData>) -> ng_gateway_sdk::NorthwardResult<()> {
        Ok(())
    }
}

/// Global one-time tracing initialization guard for Ethernet/IP integration tests.
static INIT_TRACING: Once = Once::new();

/// Initialize structured `tracing` subscriber for Ethernet/IP tests.
///
/// The configuration:
/// - Uses `DEBUG` as the maximum log level to make reconnect and timeouts visible.
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

/// Build a minimal Ethernet/IP channel, device and points for integration testing.
///
/// Parameters:
/// - `host/port/slot/timeout_ms`: PLC endpoint and timeouts.
/// - `tag_names`: tag names to read/write (e.g., `"Program:Main.MyTag"`).
pub fn build_test_topology(
    host: String,
    port: u16,
    slot: u8,
    timeout_ms: u64,
    tag_names: &[String],
    access_mode: AccessMode,
) -> (EthernetIpChannel, EthernetIpDevice, Vec<EthernetIpPoint>) {
    let connection_policy = ConnectionPolicy::default();

    let channel_config = EthernetIpChannelConfig {
        host,
        port,
        slot,
        timeout: timeout_ms,
    };

    let channel = EthernetIpChannel {
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

    let device = EthernetIpDevice {
        id: 1,
        channel_id: channel.id,
        device_name: "test-plc".to_string(),
        device_type: "ethernet-ip-simulator".to_string(),
        status: Status::Enabled,
    };

    let points: Vec<EthernetIpPoint> = tag_names
        .iter()
        .enumerate()
        .map(|(idx, tag)| EthernetIpPoint {
            id: (idx + 1) as i32,
            device_id: device.id,
            name: format!("point_{idx}"),
            key: tag.clone(),
            r#type: DataPointType::Telemetry,
            // The driver converts PLC scalar types to NGValue dynamically; for tests we keep it generic.
            data_type: DataType::String,
            access_mode,
            unit: None,
            min_value: None,
            max_value: None,
            scale: None,
            tag_name: tag.clone(),
        })
        .collect();

    (channel, device, points)
}

/// Bundle type for convenience in integration tests.
pub type InitContextBundle = (
    SouthwardInitContext,
    Arc<dyn RuntimeDevice>,
    Arc<[Arc<dyn RuntimePoint>]>,
);

/// Build a `SouthwardInitContext` and runtime device/points from the test topology.
pub fn build_init_context(
    channel: EthernetIpChannel,
    device: EthernetIpDevice,
    points: Vec<EthernetIpPoint>,
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

/// Wait until connection state becomes `Connected` or timeout.
pub async fn wait_connected(
    mut conn_rx: tokio::sync::watch::Receiver<ng_gateway_sdk::SouthwardConnectionState>,
    timeout_dur: Duration,
) -> anyhow::Result<()> {
    let res = tokio::time::timeout(timeout_dur, async move {
        loop {
            let state = conn_rx.borrow().clone();
            if matches!(state, ng_gateway_sdk::SouthwardConnectionState::Connected) {
                return Ok(());
            }
            if conn_rx.changed().await.is_err() {
                return Err(anyhow::anyhow!(
                    "connection state channel closed before becoming Connected"
                ));
            }
        }
    })
    .await;

    match res {
        Ok(inner) => inner,
        Err(_) => Err(anyhow::anyhow!(
            "timeout waiting for Ethernet/IP driver to connect; please check PLC/simulator endpoint"
        )),
    }
}
