use crate::protocol::{ChannelRuntime, PointsByDevice};
use crate::publisher::NullPublisher;
use ng_driver_opcua::{
    OpcUaAuth, OpcUaChannel, OpcUaChannelConfig, OpcUaConnector, OpcUaDevice, OpcUaPoint,
    OpcUaReadMode, SecurityMode, SecurityPolicy,
};
use ng_gateway_sdk::{
    supervision::{Connector, NoopObserverFactory, SupervisorLoop, SupervisorParams},
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, Driver, DriverResult,
    NoopSouthwardTransportMeter, ReportType, RuntimeChannel, RuntimeDevice, RuntimePoint,
    SouthwardInitContext, Status, SupervisedDriver, Transform,
};
use std::{collections::HashMap, sync::Arc};

/// Arguments for `build_opcua_channel_runtime`.
///
/// # Design
/// This struct exists to keep the benchmark harness readable and to satisfy
/// `clippy::too_many_arguments` when clippy is run with `-D warnings`.
#[derive(Debug, Clone)]
pub struct OpcuaChannelRuntimeArgs {
    /// Channel index in the benchmark scenario (0-based).
    pub channel_idx: usize,
    /// Device count per channel.
    pub devices_per_channel: usize,
    /// Point count per device.
    pub points_per_device: usize,
    /// Collection period in milliseconds.
    pub period_ms: u32,
    /// OPC UA endpoint url.
    pub endpoint_url: String,
    /// Application name.
    pub application_name: String,
    /// Application uri.
    pub application_uri: String,
    /// NodeId start for `ns=3;i=...`.
    pub node_id_start: u32,
}

/// Create an OPC UA channel runtime for benchmarks.
///
/// # Assumptions
/// - Node ids are `ns=3;i={start..start+count}` (inclusive of start, exclusive of end).
/// - All points are Float32.
pub async fn build_opcua_channel_runtime(
    args: OpcuaChannelRuntimeArgs,
) -> DriverResult<ChannelRuntime> {
    let runtime_channel: Arc<OpcUaChannel> = Arc::new(OpcUaChannel {
        id: (2000 + args.channel_idx) as i32,
        name: format!("bench-opcua-ch-{}", args.channel_idx),
        driver_id: 0,
        collection_type: CollectionType::Collection,
        report_type: ReportType::Always,
        period: Some(args.period_ms),
        status: Status::Enabled,
        connection_policy: ConnectionPolicy::default(),
        config: OpcUaChannelConfig {
            application_name: args.application_name,
            application_uri: args.application_uri,
            url: args.endpoint_url,
            auth: OpcUaAuth::Anonymous,
            security_policy: SecurityPolicy::None,
            security_mode: SecurityMode::None,
            read_mode: OpcUaReadMode::Read,
            session_timeout: 30_000,
            keep_alive_interval: 30_000,
            max_failed_keep_alive_count: 3,
            subscribe_batch_size: 256,
            max_timeouts: 3,
        },
    });

    let publisher = Arc::new(NullPublisher::default());

    let mut devices: Vec<Arc<dyn RuntimeDevice>> = Vec::with_capacity(args.devices_per_channel);
    let mut points_by_device: HashMap<i32, Vec<Arc<dyn RuntimePoint>>> =
        HashMap::with_capacity(args.devices_per_channel);

    for dev_idx in 0..args.devices_per_channel {
        // Keep ids within i32 range even for high scenario fan-out.
        let dev_id = (11_000_000i32)
            .saturating_add((args.channel_idx as i32).saturating_mul(10_000))
            .saturating_add(dev_idx as i32);
        let dev = Arc::new(OpcUaDevice {
            id: dev_id,
            channel_id: runtime_channel.id,
            device_name: format!("bench-opcua-dev-{}-{}", args.channel_idx, dev_idx),
            device_type: "bench-opcua".to_string(),
            status: Status::Enabled,
        }) as Arc<dyn RuntimeDevice>;

        let mut pts: Vec<Arc<dyn RuntimePoint>> = Vec::with_capacity(args.points_per_device);
        for i in 0..args.points_per_device {
            let nid = args.node_id_start.saturating_add(i as u32);
            let node_id = format!("ns=3;i={}", nid);
            let point = OpcUaPoint {
                id: (12_000_000i32)
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
                node_id,
            };
            pts.push(Arc::new(point) as Arc<dyn RuntimePoint>);
        }
        points_by_device.insert(dev_id, pts);
        devices.push(dev);
    }

    let mut collect_items: PointsByDevice = Vec::with_capacity(devices.len());
    let mut downlink_points: Vec<Arc<dyn RuntimePoint>> = Vec::new();

    for dev in devices.iter() {
        let dev_id = dev.id();
        let pts = points_by_device.get(&dev_id).cloned().unwrap_or_default();
        let pts_arc: Arc<[Arc<dyn RuntimePoint>]> = Arc::from(pts.into_boxed_slice());

        if downlink_points.is_empty() {
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

    let connector = <OpcUaConnector as Connector>::new(ctx).await?;
    let (loop_, _state_rx) = SupervisorLoop::new_noop_with_span(
        connector,
        SupervisorParams::default(),
        tracing::Span::current(),
    );
    let driver: SupervisedDriver<OpcUaConnector> = SupervisedDriver::new(loop_);
    let driver: Arc<dyn Driver> = Arc::new(driver);

    Ok(ChannelRuntime {
        channel_idx: args.channel_idx,
        driver,
        devices,
        points_by_device: collect_items,
        downlink_points,
    })
}
