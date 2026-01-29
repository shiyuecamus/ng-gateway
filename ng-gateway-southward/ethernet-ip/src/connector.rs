//! Ethernet/IP supervision connector.
//!
//! Final-form integration:
//! - `EthernetIpConnector`: implements SDK `Connector` and is constructed via `Connector::new(ctx)`.
//! - `connect()`: creates a connected `EipClient` and returns `EthernetIpSession`.

use super::{
    handle::EthernetIpHandle,
    session::EthernetIpSession,
    types::{EthernetIpChannel, EthernetIpChannelConfig},
};
use ng_gateway_sdk::{
    connect_tcp_metered_with_timeout,
    supervision::{Connector, Session, SessionContext},
    DriverError, DriverResult, FailureKind, FailurePhase, SouthwardInitContext,
    SouthwardTransportMeter,
};
use rust_ethernet_ip::{EipClient, RoutePath};
use std::sync::Arc;

/// Ethernet/IP connector built from `SouthwardInitContext` (no I/O).
#[derive(Clone)]
pub struct EthernetIpConnector {
    channel: Arc<EthernetIpChannel>,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
    handle: Arc<EthernetIpHandle>,
}

impl EthernetIpConnector {
    async fn connect_once(
        &self,
        ctx: &SessionContext,
        cfg: &EthernetIpChannelConfig,
    ) -> DriverResult<EipClient> {
        let addr = format!("{}:{}", cfg.host, cfg.port);
        let slot = cfg.slot;
        let socket_addr = addr.parse::<std::net::SocketAddr>().map_err(|e| {
            DriverError::ConfigurationError(format!("Invalid Ethernet/IP address {addr}: {e}"))
        })?;

        let fut = connect_tcp_metered_with_timeout(
            socket_addr,
            Arc::clone(&self.transport_meter),
            self.channel.connection_policy.connect_timeout_ms,
        );
        let io = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                return Err(DriverError::ServiceUnavailable);
            }
            res = fut => res.map_err(|e| DriverError::SessionError(format!("Ethernet/IP TCP connect failed: {e}")))?,
        };

        // Best-effort TCP_NODELAY for small CIP packets.
        let _ = io.inner_ref().set_nodelay(true);

        let route = if slot == 0 {
            None
        } else {
            Some(RoutePath::new().add_slot(slot))
        };

        EipClient::connect_with_stream(io, route)
            .await
            .map_err(|e| {
                DriverError::SessionError(format!("Failed to connect to {addr} (Slot {slot}): {e}"))
            })
    }
}

#[async_trait::async_trait]
impl Connector for EthernetIpConnector {
    type InitContext = SouthwardInitContext;
    type Handle = EthernetIpHandle;
    type Session = EthernetIpSession;

    fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        let channel = ctx
            .runtime_channel
            .downcast_arc::<EthernetIpChannel>()
            .map_err(|_| {
                DriverError::ConfigurationError("Invalid EthernetIpChannel".to_string())
            })?;
        let handle = Arc::new(EthernetIpHandle::new(Arc::clone(&channel)));
        Ok(Self {
            channel,
            transport_meter: ctx.transport_meter,
            handle,
        })
    }

    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let client = self.connect_once(&ctx, &self.channel.config).await?;
        Ok(EthernetIpSession::new(Arc::clone(&self.handle), client))
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        _err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        FailureKind::Retryable
    }
}
