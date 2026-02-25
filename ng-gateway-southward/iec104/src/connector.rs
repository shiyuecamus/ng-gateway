//! IEC104 supervised connector + session implementation.
//!
//! Final-form integration:
//! - `Iec104Connector`: implements SDK `Connector` and is constructed via `Iec104Connector::new(ctx)`.
//! - `Iec104Session`: implements SDK `Session` lifecycle (init/run) and publishes `Iec104Handle`.
use super::{
    handle::Iec104Handle,
    protocol::session::{create_with_stream, SessionConfig},
    session::Iec104Session,
    types::Iec104Channel,
};
use ng_gateway_sdk::{
    connect_tcp_metered_with_timeout,
    supervision::{Connector, Session, SessionContext},
    DriverError, DriverResult, FailureKind, FailurePhase, NoopSouthwardTransportMeter,
    SouthwardInitContext, SouthwardTransportMeter,
};
use std::net::SocketAddr;
use std::sync::Arc;

/// IEC104 supervised connector (constructed from init context, no I/O).
#[derive(Clone)]
pub struct Iec104Connector {
    handle: Arc<Iec104Handle>,
    channel: Arc<Iec104Channel>,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
}

impl Iec104Connector {
    /// Create the connector from init context (no I/O).
    pub fn from_init(ctx: SouthwardInitContext) -> DriverResult<Self> {
        let transport_meter = ctx
            .extensions
            .get_or_default(|| Arc::new(NoopSouthwardTransportMeter) as Arc<dyn SouthwardTransportMeter>);
        let channel = Arc::clone(&ctx.runtime_channel)
            .downcast_arc::<Iec104Channel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid Iec104Channel".to_string()))?;

        let ca_to_snapshot = Iec104Handle::build_mappings(&ctx);
        let handle = Arc::new(Iec104Handle::new(
            Arc::clone(&channel),
            Arc::clone(&ctx.publisher),
            ca_to_snapshot,
        ));

        Ok(Self {
            handle,
            channel,
            transport_meter,
        })
    }

    #[inline]
    fn socket_addr(&self) -> DriverResult<SocketAddr> {
        let socket_addr = format!("{}:{}", self.channel.config.host, self.channel.config.port)
            .parse::<SocketAddr>()
            .map_err(|e| DriverError::ConfigurationError(format!("Invalid socket address: {e}")))?;
        Ok(socket_addr)
    }

    #[inline]
    fn build_session_config(&self) -> SessionConfig {
        SessionConfig {
            connection_timeout_ms: self.channel.connection_policy.connect_timeout_ms,
            t0_ms: self.channel.config.t0_ms as u64,
            t1_ms: self.channel.config.t1_ms as u64,
            t2_ms: self.channel.config.t2_ms as u64,
            t3_ms: self.channel.config.t3_ms as u64,
            k_window: self.channel.config.k_window,
            w_threshold: self.channel.config.w_threshold,
            send_queue_capacity: self.channel.config.send_queue_capacity,
            tcp_nodelay: self.channel.config.tcp_nodelay,
            max_pending_asdu_bytes: self.channel.config.max_pending_asdu_bytes,
            discard_low_priority_when_window_full: self
                .channel
                .config
                .discard_low_priority_when_window_full,
            merge_low_priority: self.channel.config.merge_low_priority,
            low_prio_flush_max_age_ms: self.channel.config.low_prio_flush_max_age_ms,
        }
    }
}

#[async_trait::async_trait]
impl Connector for Iec104Connector {
    type InitContext = SouthwardInitContext;
    type Handle = Iec104Handle;
    type Session = Iec104Session;

    #[inline]
    fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        Self::from_init(ctx)
    }

    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let socket_addr = self.socket_addr()?;
        let config = self.build_session_config();

        let connect_fut = connect_tcp_metered_with_timeout(
            socket_addr,
            Arc::clone(&self.transport_meter),
            config.connection_timeout_ms,
        );

        let stream = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                return Err(DriverError::ServiceUnavailable);
            }
            res = connect_fut => res.map_err(|e| DriverError::ExecutionError(e.to_string()))?,
        };

        let _ = stream.inner_ref().set_nodelay(config.tcp_nodelay);
        let (proto_session, event_loop) = create_with_stream(socket_addr, config.clone(), stream);

        let startup_actions = self.handle.build_startup_actions();

        Ok(Iec104Session {
            handle: Arc::clone(&self.handle),
            channel: Arc::clone(&self.channel),
            proto_session,
            event_loop: Some(event_loop),
            event_loop_cancel: None,
            event_loop_join: None,
            startup_actions,
        })
    }

    fn classify_error(
        &self,
        phase: FailurePhase,
        err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        let _ = phase;
        match err {
            DriverError::ConfigurationError(_) | DriverError::ValidationError(_) => {
                FailureKind::Fatal
            }
            _ => FailureKind::Retryable,
        }
    }
}
