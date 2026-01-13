use crate::{
    handler::Dnp3SoeHandler,
    types::{Dnp3Channel, Dnp3Connection, Dnp3PointGroup, PointMeta},
};
use arc_swap::ArcSwap;
use backoff::backoff::Backoff;
use dashmap::DashMap;
use dnp3::{
    app::{ConnectStrategy, Listener, MaybeAsync, Timeout, Variation},
    link::{EndpointAddress, LinkErrorMode, LinkReadMode},
    master::{
        AssociationConfig, AssociationHandle, AssociationHandler, AssociationInformation, Classes,
        EventClasses, MasterChannelConfig, ReadHeader, ReadRequest,
    },
    serial::{spawn_master_serial, PortState, SerialSettings},
    tcp::{spawn_master_tcp_client, ClientState, EndpointList},
    udp::spawn_master_udp,
};
use ng_gateway_sdk::{
    build_exponential_backoff, DriverError, DriverResult, NorthwardPublisher,
    SouthwardConnectionState,
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;
use tokio::time::{interval_at, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

pub type SharedAssociation = Arc<ArcSwap<Option<AssociationHandle>>>;

struct StateListener {
    conn_tx: Arc<watch::Sender<SouthwardConnectionState>>,
}

impl Listener<ClientState> for StateListener {
    fn update(&mut self, state: ClientState) -> MaybeAsync<()> {
        info!("DNP3 TCP Client State: {:?}", state);
        let status = match state {
            ClientState::Connected => SouthwardConnectionState::Connected,
            ClientState::Connecting
            | ClientState::WaitAfterFailedConnect(_)
            | ClientState::WaitAfterDisconnect(_) => SouthwardConnectionState::Connecting,
            ClientState::Disabled | ClientState::Shutdown => SouthwardConnectionState::Disconnected,
        };
        let _ = self.conn_tx.send(status);
        MaybeAsync::ready(())
    }
}

struct PortListener {
    conn_tx: Arc<watch::Sender<SouthwardConnectionState>>,
}
impl Listener<PortState> for PortListener {
    fn update(&mut self, state: PortState) -> MaybeAsync<()> {
        info!("DNP3 Serial Port State: {:?}", state);
        let status = match state {
            PortState::Open => SouthwardConnectionState::Connected,
            PortState::Wait(_) => SouthwardConnectionState::Reconnecting,
            PortState::Shutdown => SouthwardConnectionState::Failed("shutdown".to_string()),
            PortState::Disabled => SouthwardConnectionState::Disconnected,
        };
        let _ = self.conn_tx.send(status);
        MaybeAsync::ready(())
    }
}

struct NoOpAssociationHandler;
impl AssociationHandler for NoOpAssociationHandler {}

struct NoOpAssociationInformation;
impl AssociationInformation for NoOpAssociationInformation {}

pub struct Dnp3Supervisor {
    channel: Arc<Dnp3Channel>,
    points_map: Arc<DashMap<(Dnp3PointGroup, u16), PointMeta>>,
    publisher: Arc<dyn NorthwardPublisher>,
    shared_association: SharedAssociation,
    conn_tx: watch::Sender<SouthwardConnectionState>,
    /// Cancellation token controlling the supervisor lifecycle.
    cancel: CancellationToken,
}

impl Dnp3Supervisor {
    pub fn new(
        channel: Arc<Dnp3Channel>,
        points_map: Arc<DashMap<(Dnp3PointGroup, u16), PointMeta>>,
        publisher: Arc<dyn NorthwardPublisher>,
        shared_association: SharedAssociation,
        conn_tx: watch::Sender<SouthwardConnectionState>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            channel,
            points_map,
            publisher,
            shared_association,
            conn_tx,
            cancel,
        }
    }

    /// Start the supervisor task.
    ///
    /// This spawns the supervisor loop on the current runtime.
    pub async fn run(self) -> DriverResult<()> {
        // Pre-validate configuration to fail fast
        match &self.channel.config.connection {
            Dnp3Connection::Tcp { host, port } => {
                if host.trim().is_empty() || *port == 0 {
                    return Err(DriverError::ConfigurationError(format!(
                        "Invalid TCP endpoint configuration host='{}' port={}",
                        host, port
                    )));
                }
            }
            Dnp3Connection::Udp { .. } => {}
            Dnp3Connection::Serial { .. } => {}
        }

        // Validate and construct link-layer addresses once up‑front so we can
        // reuse them inside the supervisor loop without additional error
        // handling or panics.
        let master_addr =
            EndpointAddress::try_new(self.channel.config.local_addr).map_err(|e| {
                DriverError::ConfigurationError(format!(
                    "Invalid local (master) address {}: {}",
                    self.channel.config.local_addr, e
                ))
            })?;

        let remote_addr =
            EndpointAddress::try_new(self.channel.config.remote_addr).map_err(|e| {
                DriverError::ConfigurationError(format!(
                    "Invalid remote (outstation) address {}: {}",
                    self.channel.config.remote_addr, e
                ))
            })?;

        // Subscribe to connection state updates so that we can gate polling
        // logic based on the latest state. This avoids unnecessary read
        // requests and noisy logs when the DNP3 channel is disconnected.
        let conn_rx = self.conn_tx.subscribe();

        tokio::spawn(async move {
            // Connection state receiver used inside the supervisor loop.
            // It is kept local to the spawned task to avoid holding any
            // unnecessary references on the main thread.
            let mut conn_rx = conn_rx;

            // Clone immutable configuration required inside the async loop.
            let master_addr = master_addr;
            let remote_addr = remote_addr;

            // Build an exponential backoff strategy based on the shared
            // `ConnectionPolicy` so that DNP3 behaves consistently with
            // other southbound drivers (Modbus, S7, DL/T 645, etc.).
            let mut backoff = build_exponential_backoff(&self.channel.connection_policy.backoff);
            let mut attempt: u64 = 0;

            loop {
                if self.cancel.is_cancelled() {
                    let _ = self
                        .conn_tx
                        .send(SouthwardConnectionState::Failed("cancelled".to_string()));
                    return;
                }

                info!("Starting DNP3 Master Supervisor (attempt={})", attempt + 1);
                let _ = self.conn_tx.send(SouthwardConnectionState::Connecting);

                // Perform a single connect + association lifecycle.
                // This mirrors the `connect_once` pattern from other drivers.
                match Self::connect_once(&self, master_addr, remote_addr, &mut conn_rx).await {
                    Ok(mut association) => {
                        // Association established successfully; run the polling loop
                        // until either cancellation or a fatal error occurs.
                        if let Err(e) = Self::poll_loop(&self, &mut association, &mut conn_rx).await
                        {
                            error!("DNP3 polling loop failed: {:?}", e);
                            let _ = self.conn_tx.send(SouthwardConnectionState::Failed(format!(
                                "poll loop failed: {:?}",
                                e
                            )));
                        } else {
                            // Normal termination (e.g. cancellation) – exit supervisor.
                            return;
                        }
                    }
                    Err(e) => {
                        error!("DNP3 connect_once failed: {:?}", e);
                        let _ = self.conn_tx.send(SouthwardConnectionState::Failed(format!(
                            "connect_once failed: {:?}",
                            e
                        )));

                        attempt = attempt.saturating_add(1);
                        let delay = backoff.next_backoff().unwrap_or_else(|| {
                            Duration::from_millis(
                                self.channel.connection_policy.backoff.max_interval_ms,
                            )
                        });

                        warn!(
                            attempt = attempt,
                            delay_ms = delay.as_millis() as u64,
                            "DNP3 supervisor retry after failure"
                        );

                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {}
                            _ = self.cancel.cancelled() => {
                                let _ = self.conn_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                                return;
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// Perform a single DNP3 master session lifecycle:
    /// - create master channel (TCP/UDP/Serial)
    /// - enable the channel
    /// - add association
    ///
    /// This mirrors the `connect_once` pattern used by other southbound
    /// drivers so that session setup errors can be retried with a shared
    /// exponential backoff strategy in the outer supervisor loop. The
    /// returned association handle is then driven by `poll_loop`.
    async fn connect_once(
        &self,
        master_addr: EndpointAddress,
        remote_addr: EndpointAddress,
        _conn_rx: &mut watch::Receiver<SouthwardConnectionState>,
    ) -> DriverResult<AssociationHandle> {
        // Create Master Channel and association depending on the physical transport.
        //
        // TCP / TLS and Serial transports use the standard `add_association` API
        // which expects a stream-oriented link layer, while UDP requires the
        // dedicated `add_udp_association` API that also carries the remote
        // socket address. Using the wrong association method on a UDP channel
        // results in `WrongChannelType { actual: Udp, required: Stream }`.
        let association = match &self.channel.config.connection {
            Dnp3Connection::Tcp { host, port } => {
                let endpoint = format!("{}:{}", host, port);
                let policy = &self.channel.connection_policy;
                let endpoints = EndpointList::single(endpoint);

                // Map gateway ConnectionPolicy to the DNP3 ConnectStrategy so that TCP
                // reconnection follows the same exponential backoff parameters.
                let connect_strategy = ConnectStrategy::new(
                    Duration::from_millis(policy.backoff.initial_interval_ms.max(1)),
                    Duration::from_millis(
                        policy
                            .backoff
                            .max_interval_ms
                            .max(policy.backoff.initial_interval_ms),
                    ),
                    Duration::from_millis(policy.backoff.initial_interval_ms.max(1)),
                );

                let master_config = MasterChannelConfig::new(master_addr);

                let mut master_channel = spawn_master_tcp_client(
                    LinkErrorMode::Close,
                    master_config,
                    endpoints,
                    connect_strategy,
                    Box::new(StateListener {
                        conn_tx: Arc::new(self.conn_tx.clone()),
                    }),
                );

                // Enable master channel
                if let Err(e) = master_channel.enable().await {
                    return Err(DriverError::SessionError(format!(
                        "Failed to enable TCP master channel: {:?}",
                        e
                    )));
                }

                // Build a shared association configuration for stream-like transports.
                let association_config = AssociationConfig {
                    // Always enable unsolicited responses for event-driven reporting.
                    enable_unsol_classes: EventClasses::all(),
                    disable_unsol_classes: EventClasses::none(),
                    ..AssociationConfig::default()
                };

                let read_handler = Box::new(Dnp3SoeHandler::new(
                    self.points_map.clone(),
                    self.publisher.clone(),
                ));

                master_channel
                    .add_association(
                        remote_addr,
                        association_config,
                        read_handler,
                        Box::new(NoOpAssociationHandler),
                        Box::new(NoOpAssociationInformation),
                    )
                    .await
                    .map_err(|e| {
                        DriverError::SessionError(format!(
                            "Failed to add DNP3 TCP association: {:?}",
                            e
                        ))
                    })?
            }
            Dnp3Connection::Udp {
                host,
                port,
                local_port,
            } => {
                let local_ip: IpAddr = IpAddr::from([0, 0, 0, 0]);
                let bind_port = local_port.unwrap_or(0);
                let local_endpoint = SocketAddr::new(local_ip, bind_port);

                // Remote UDP endpoint is required to route application frames.
                let remote_endpoint: SocketAddr =
                    format!("{}:{}", host, port).parse().map_err(|e| {
                        DriverError::ConfigurationError(format!(
                            "Invalid UDP remote endpoint {}:{}: {}",
                            host, port, e
                        ))
                    })?;

                warn!(
                    "Starting DNP3 UDP Master on {}. Remote target {}",
                    local_endpoint, remote_endpoint
                );

                let master_config = MasterChannelConfig::new(master_addr);

                let mut master_channel = spawn_master_udp(
                    local_endpoint,
                    LinkReadMode::Datagram,
                    Timeout::default(),
                    master_config,
                );

                if let Err(e) = master_channel.enable().await {
                    return Err(DriverError::SessionError(format!(
                        "Failed to enable UDP master channel: {:?}",
                        e
                    )));
                }

                let association_config = AssociationConfig {
                    // Always enable unsolicited responses for event-driven reporting.
                    enable_unsol_classes: EventClasses::all(),
                    disable_unsol_classes: EventClasses::none(),
                    ..AssociationConfig::default()
                };

                let read_handler = Box::new(Dnp3SoeHandler::new(
                    self.points_map.clone(),
                    self.publisher.clone(),
                ));

                let assoc = master_channel
                    .add_udp_association(
                        remote_addr,
                        remote_endpoint,
                        association_config,
                        read_handler,
                        Box::new(NoOpAssociationHandler),
                        Box::new(NoOpAssociationInformation),
                    )
                    .await
                    .map_err(|e| {
                        DriverError::SessionError(format!(
                            "Failed to add DNP3 UDP association: {:?}",
                            e
                        ))
                    })?;

                // For UDP we don't have a state listener, so we mark the connection
                // as established once the association is successfully created.
                let _ = self.conn_tx.send(SouthwardConnectionState::Connected);

                assoc
            }
            Dnp3Connection::Serial {
                path,
                baud_rate,
                data_bits,
                stop_bits,
                parity,
            } => {
                let master_config = MasterChannelConfig::new(master_addr);
                let settings = SerialSettings {
                    baud_rate: *baud_rate,
                    data_bits: (*data_bits).into(),
                    stop_bits: (*stop_bits).into(),
                    parity: (*parity).into(),
                    flow_control: dnp3::serial::FlowControl::None,
                };

                let mut master_channel = spawn_master_serial(
                    master_config,
                    path,
                    settings,
                    Duration::from_secs(5),
                    Box::new(PortListener {
                        conn_tx: Arc::new(self.conn_tx.clone()),
                    }),
                );

                if let Err(e) = master_channel.enable().await {
                    return Err(DriverError::SessionError(format!(
                        "Failed to enable Serial master channel: {:?}",
                        e
                    )));
                }

                let association_config = AssociationConfig {
                    // Always enable unsolicited responses for event-driven reporting.
                    enable_unsol_classes: EventClasses::all(),
                    disable_unsol_classes: EventClasses::none(),
                    ..AssociationConfig::default()
                };

                let read_handler = Box::new(Dnp3SoeHandler::new(
                    self.points_map.clone(),
                    self.publisher.clone(),
                ));

                master_channel
                    .add_association(
                        remote_addr,
                        association_config,
                        read_handler,
                        Box::new(NoOpAssociationHandler),
                        Box::new(NoOpAssociationInformation),
                    )
                    .await
                    .map_err(|e| {
                        DriverError::SessionError(format!(
                            "Failed to add DNP3 Serial association: {:?}",
                            e
                        ))
                    })?
            }
        };

        info!("DNP3 Association added");
        self.shared_association
            .store(Arc::new(Some(association.clone())));

        Ok(association)
    }

    /// Drive the association by performing periodic integrity and event
    /// scans until either cancellation occurs or a fatal error happens.
    ///
    /// On cancellation, this method returns `Ok(())` so that the outer
    /// supervisor can terminate cleanly. On a fatal polling error it
    /// returns `Err`, triggering exponential backoff in the supervisor.
    async fn poll_loop(
        &self,
        association: &mut AssociationHandle,
        conn_rx: &mut watch::Receiver<SouthwardConnectionState>,
    ) -> DriverResult<()> {
        // Initial Integrity Poll (Class 0 + Events)
        if let Err(e) = association
            .read(ReadRequest::class_scan(Classes::all()))
            .await
        {
            warn!("Initial integrity poll failed: {:?}", e);
        }

        // Periodic Poll Loop
        let mut interval_integrity = interval_at(
            Instant::now(),
            Duration::from_millis(self.channel.config.integrity_scan_interval_ms),
        );

        let mut interval_event = interval_at(
            Instant::now(),
            Duration::from_millis(self.channel.config.event_scan_interval_ms),
        );

        // Clone handles for concurrent use in select! loop
        let mut integrity_assoc = association.clone();
        let mut event_assoc = association.clone();

        // Monitor loop: drive integrity and event scans while respecting
        // the current connection state and cancellation token.
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    let _ = self
                        .conn_tx
                        .send(SouthwardConnectionState::Failed("cancelled".to_string()));
                    return Ok(());
                }
                _ = interval_integrity.tick() => {
                    // Only perform integrity scans when the underlying connection
                    // is in a Connected state. This prevents unnecessary polling
                    // and repeated NoConnection warnings while the DNP3 master
                    // is still reconnecting according to its backoff strategy.
                    let is_connected = matches!(
                        *conn_rx.borrow(),
                        SouthwardConnectionState::Connected
                    );
                    if !is_connected {
                        continue;
                    }

                    // Construct integrity scan request
                    // 1. Class 0123 (Integrity + Events) - Mapped to Group 60 Var 1/2/3/4
                    // 2. Group 110 Var 0 (All Octet Strings)
                    let headers = vec![
                        ReadHeader::all_objects(Variation::Group60Var1), // Class 0 (Static)
                        ReadHeader::all_objects(Variation::Group60Var2), // Class 1 (Events)
                        ReadHeader::all_objects(Variation::Group60Var3), // Class 2 (Events)
                        ReadHeader::all_objects(Variation::Group60Var4), // Class 3 (Events)
                        ReadHeader::all_objects(Variation::Group110(0)), // All Octet Strings
                    ];

                    if let Err(e) = integrity_assoc.read(ReadRequest::multiple_headers(&headers)).await {
                         warn!("Integrity scan failed: {:?}", e);
                    }
                }
                _ = interval_event.tick() => {
                    // Only perform event scans when we are connected. When the
                    // transport is disconnected, the DNP3 master will not be able
                    // to fulfill the request and would otherwise generate a
                    // stream of NoConnection warnings. Gating on connection
                    // state keeps logs clean and reduces needless work.
                    let is_connected = matches!(
                        *conn_rx.borrow(),
                        SouthwardConnectionState::Connected
                    );
                    if !is_connected {
                        continue;
                    }

                    // Read Class 1, 2, 3
                    if let Err(e) = event_assoc.read(ReadRequest::class_scan(Classes::class123())).await {
                         warn!("Event scan failed: {:?}", e);
                         return Err(DriverError::SessionError(format!(
                             "DNP3 event scan failed: {:?}",
                             e
                         )));
                    }
                }
            }
        }
    }
}
