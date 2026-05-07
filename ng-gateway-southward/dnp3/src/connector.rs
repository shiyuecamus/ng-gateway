//! DNP3 supervision connector.
//!
//! Final-form integration:
//! - `Dnp3Connector`: implements SDK `Connector` and is constructed via `Connector::new(ctx)`.
//! - `connect()`: establishes master channel + association and returns an attempt `Dnp3Session`.

use super::{
    handle::Dnp3Handle,
    handler::Dnp3SoeHandler,
    session::Dnp3Session,
    types::{Dnp3Channel, Dnp3Connection},
};
use dnp3::{
    app::{ConnectStrategy, Listener, MaybeAsync, Timeout},
    link::{EndpointAddress, LinkErrorMode, LinkReadMode},
    master::{
        AssociationConfig, AssociationHandle, AssociationHandler, AssociationInformation,
        EventClasses, MasterChannelConfig,
    },
    serial::{spawn_master_serial, PortState, SerialSettings},
    tcp::{spawn_master_tcp_client, ClientState, EndpointList},
    udp::spawn_master_udp,
};
use ng_gateway_sdk::{
    supervision::{Connector, Session, SessionContext},
    DriverError, DriverResult, FailureKind, FailurePhase, SouthwardInitContext,
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

struct NoOpAssociationHandler;
impl AssociationHandler for NoOpAssociationHandler {}

struct NoOpAssociationInformation;
impl AssociationInformation for NoOpAssociationInformation {}

struct NoopStateListener;
impl Listener<ClientState> for NoopStateListener {
    fn update(&mut self, _state: ClientState) -> MaybeAsync<()> {
        MaybeAsync::ready(())
    }
}

struct NoopPortListener;
impl Listener<PortState> for NoopPortListener {
    fn update(&mut self, _state: PortState) -> MaybeAsync<()> {
        MaybeAsync::ready(())
    }
}

/// DNP3 connector built from `SouthwardInitContext` (no I/O).
#[derive(Clone)]
pub struct Dnp3Connector {
    channel: Arc<Dnp3Channel>,
    handle: Arc<Dnp3Handle>,
}

impl Dnp3Connector {
    async fn connect_once(&self, ctx: &SessionContext) -> DriverResult<AssociationHandle> {
        let cfg = &self.channel.config;

        let master_addr = EndpointAddress::try_new(cfg.local_addr).map_err(|e| {
            DriverError::ConfigurationError(format!(
                "Invalid local (master) address {}: {}",
                cfg.local_addr, e
            ))
        })?;
        let remote_addr = EndpointAddress::try_new(cfg.remote_addr).map_err(|e| {
            DriverError::ConfigurationError(format!(
                "Invalid remote (outstation) address {}: {}",
                cfg.remote_addr, e
            ))
        })?;

        let association_config = AssociationConfig {
            enable_unsol_classes: EventClasses::all(),
            disable_unsol_classes: EventClasses::none(),
            ..AssociationConfig::default()
        };

        let read_handler = Box::new(Dnp3SoeHandler::new(
            Arc::clone(&self.handle.points_map),
            Arc::clone(&self.handle.publisher),
        ));

        let res = match &cfg.connection {
            Dnp3Connection::Tcp { host, port } => {
                let endpoint = format!("{}:{}", host, port);
                let policy = &self.channel.connection_policy;
                let endpoints = EndpointList::single(endpoint);

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
                    Box::new(NoopStateListener),
                );

                tokio::select! {
                    _ = ctx.cancel.cancelled() => { return Err(DriverError::ServiceUnavailable); }
                    r = master_channel.enable() => {
                        r.map_err(|e| DriverError::SessionError(format!("Failed to enable TCP master channel: {:?}", e)))?;
                    }
                }

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
                    })
            }
            Dnp3Connection::Udp {
                host,
                port,
                local_port,
            } => {
                let local_ip: IpAddr = IpAddr::from([0, 0, 0, 0]);
                let bind_port = local_port.unwrap_or(0);
                let local_endpoint = SocketAddr::new(local_ip, bind_port);
                let remote_endpoint: SocketAddr =
                    format!("{}:{}", host, port).parse().map_err(|e| {
                        DriverError::ConfigurationError(format!(
                            "Invalid UDP remote endpoint {}:{}: {}",
                            host, port, e
                        ))
                    })?;

                let master_config = MasterChannelConfig::new(master_addr);
                let mut master_channel = spawn_master_udp(
                    local_endpoint,
                    LinkReadMode::Datagram,
                    Timeout::default(),
                    master_config,
                );

                tokio::select! {
                    _ = ctx.cancel.cancelled() => { return Err(DriverError::ServiceUnavailable); }
                    r = master_channel.enable() => {
                        r.map_err(|e| DriverError::SessionError(format!("Failed to enable UDP master channel: {:?}", e)))?;
                    }
                }

                master_channel
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
                    })
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
                    Box::new(NoopPortListener),
                );

                tokio::select! {
                    _ = ctx.cancel.cancelled() => { return Err(DriverError::ServiceUnavailable); }
                    r = master_channel.enable() => {
                        r.map_err(|e| DriverError::SessionError(format!("Failed to enable Serial master channel: {:?}", e)))?;
                    }
                }

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
                    })
            }
        };

        match res {
            Ok(assoc) => Ok(assoc),
            Err(e) => Err(e),
        }
    }
}

#[async_trait::async_trait]
impl Connector for Dnp3Connector {
    type InitContext = SouthwardInitContext;
    type Handle = Dnp3Handle;
    type Session = Dnp3Session;

    async fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        let channel = Arc::clone(&ctx.runtime_channel)
            .downcast_arc::<Dnp3Channel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid Dnp3Channel".to_string()))?;
        let handle = Arc::new(Dnp3Handle::new(
            Arc::clone(&channel),
            Arc::clone(&ctx.publisher),
        ));
        handle.build_indexes(&ctx);
        Ok(Self { channel, handle })
    }

    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let assoc = self.connect_once(&ctx).await?;
        Ok(Dnp3Session::new(Arc::clone(&self.handle), assoc))
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        _err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        FailureKind::Retryable
    }
}
