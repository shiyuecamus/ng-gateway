//! S7 supervision connector.
//!
//! Final-form integration:
//! - `S7Connector`: implements SDK `Connector` and is constructed via `Connector::new(ctx)`.
//! - `connect()`: establishes transport and returns an attempt-scoped `S7Session`.

use super::{
    handle::S7Handle,
    protocol::{
        frame::default_tsap_pair,
        session::{create_with_stream, SessionConfig},
    },
    session::S7Session,
    types::{S7Channel, TsapConfig},
};
use ng_gateway_sdk::{
    connect_tcp_metered_with_timeout,
    supervision::{Connector, Session, SessionContext},
    DriverError, DriverResult, FailureKind, FailurePhase, SouthwardInitContext,
    SouthwardTransportMeter,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};

/// S7 connector built from `SouthwardInitContext` (no I/O).
#[derive(Clone)]
pub struct S7Connector {
    channel: Arc<S7Channel>,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
    handle: Arc<S7Handle>,
}

impl S7Connector {
    #[inline]
    fn socket_addr(&self) -> DriverResult<SocketAddr> {
        format!("{}:{}", self.channel.config.host, self.channel.config.port)
            .parse::<SocketAddr>()
            .map_err(|e| DriverError::ConfigurationError(format!("Invalid socket address: {e}")))
    }

    #[inline]
    fn build_session_config(&self, socket_addr: SocketAddr) -> DriverResult<Arc<SessionConfig>> {
        let (src, dst) = match self.channel.config.tsap {
            TsapConfig::RackSlot { rack, slot } => {
                let pair = default_tsap_pair(self.channel.config.cpu, rack, slot)
                    .map_err(|e| DriverError::ConfigurationError(format!("Invalid TSAP: {e}")))?;
                (pair.local.into(), pair.remote.into())
            }
            TsapConfig::Tsap { src, dst } => (src, dst),
        };

        Ok(Arc::new(SessionConfig {
            socket_addr,
            cpu: self.channel.config.cpu,
            tsap_src: src,
            tsap_dst: dst,
            preferred_pdu_size: self.channel.config.preferred_pdu_size,
            preferred_amq_caller: self.channel.config.preferred_amq_caller,
            preferred_amq_callee: self.channel.config.preferred_amq_callee,
            connect_timeout: Duration::from_millis(
                self.channel.connection_policy.connect_timeout_ms,
            ),
            read_timeout: Duration::from_millis(self.channel.connection_policy.read_timeout_ms),
            write_timeout: Duration::from_millis(self.channel.connection_policy.write_timeout_ms),
            ..Default::default()
        }))
    }
}

#[async_trait::async_trait]
impl Connector for S7Connector {
    type InitContext = SouthwardInitContext;
    type Handle = S7Handle;
    type Session = S7Session;

    #[inline]
    async fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        let channel = ctx
            .runtime_channel
            .downcast_arc::<S7Channel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid S7Channel".to_string()))?;
        let handle = Arc::new(S7Handle::new(Arc::clone(&channel)));
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
        let config = self.build_session_config(socket_addr)?;

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
        Ok(S7Session::new(
            Arc::clone(&self.handle),
            proto_session,
            event_loop,
        ))
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        _error: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        FailureKind::Retryable
    }
}
