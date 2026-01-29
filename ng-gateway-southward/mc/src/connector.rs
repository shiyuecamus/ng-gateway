//! MC supervision connector.
//!
//! Final-form integration:
//! - `McConnector`: implements SDK `Connector` and is constructed via `Connector::new(ctx)`.
//! - `connect()`: establishes transport and returns an attempt-scoped `McSession`.

use super::{
    handle::McHandle,
    protocol::session::{create_with_stream, SessionConfig},
    session::McSession,
    types::McChannel,
};
use ng_gateway_sdk::{
    connect_tcp_metered_with_timeout,
    supervision::{Connector, Session, SessionContext},
    DriverError, DriverResult, FailureKind, FailurePhase, SouthwardInitContext,
    SouthwardTransportMeter,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};

/// MC connector built from `SouthwardInitContext` (no I/O).
#[derive(Clone)]
pub struct McConnector {
    channel: Arc<McChannel>,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
    handle: Arc<McHandle>,
}

impl McConnector {
    #[inline]
    fn socket_addr(&self) -> DriverResult<SocketAddr> {
        format!("{}:{}", self.channel.config.host, self.channel.config.port)
            .parse::<SocketAddr>()
            .map_err(|e| DriverError::ConfigurationError(format!("Invalid socket address: {e}")))
    }

    #[inline]
    fn build_session_config(&self, socket_addr: SocketAddr) -> Arc<SessionConfig> {
        Arc::new(SessionConfig {
            socket_addr,
            series: self.channel.config.series,
            frame_variant: self.channel.config.series.frame_variant(),
            connect_timeout: Duration::from_millis(
                self.channel.connection_policy.connect_timeout_ms,
            ),
            read_timeout: Duration::from_millis(self.channel.connection_policy.read_timeout_ms),
            write_timeout: Duration::from_millis(self.channel.connection_policy.write_timeout_ms),
            send_queue_capacity: 256,
            max_concurrent_requests: self.channel.config.concurrent_requests.unwrap_or(1).max(1)
                as usize,
            tcp_nodelay: true,
        })
    }
}

#[async_trait::async_trait]
impl Connector for McConnector {
    type InitContext = SouthwardInitContext;
    type Handle = McHandle;
    type Session = McSession;

    #[inline]
    fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        let channel = ctx
            .runtime_channel
            .downcast_arc::<McChannel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid McChannel".to_string()))?;
        let handle = Arc::new(McHandle::new(Arc::clone(&channel)));
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
        let socket_addr = self.socket_addr()?;
        let config = self.build_session_config(socket_addr);

        let connect_fut = connect_tcp_metered_with_timeout(
            socket_addr,
            Arc::clone(&self.transport_meter),
            self.channel.connection_policy.connect_timeout_ms,
        );
        let stream = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                return Err(DriverError::ServiceUnavailable);
            }
            res = connect_fut => res.map_err(|e| DriverError::ExecutionError(e.to_string()))?,
        };
        let _ = stream.inner_ref().set_nodelay(true);

        let (proto_session, event_loop) = create_with_stream(config, stream);
        Ok(McSession::new(
            Arc::clone(&self.handle),
            proto_session,
            event_loop,
        ))
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        _err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        FailureKind::Retryable
    }
}
