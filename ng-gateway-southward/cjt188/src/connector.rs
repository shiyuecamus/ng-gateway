//! CJ/T 188 supervision connector.
//!
//! Final-form integration:
//! - `Cjt188Connector`: implements SDK `Connector` and is constructed via `Connector::new(ctx)`.
//! - `connect()`: establishes transport and returns an attempt-scoped `Cjt188Session`.

use super::{
    handle::Cjt188Handle,
    protocol::session::{Cjt188Session as ProtoSession, Cjt188SessionImpl, SessionConfig},
    session::Cjt188Session,
    types::{Cjt188Channel, Cjt188Connection},
};
use ng_gateway_sdk::{
    connect_serial_metered, connect_tcp_metered_with_timeout,
    supervision::{Connector, Session, SessionContext},
    DriverError, DriverResult, FailureKind, FailurePhase, SerialConnectConfig,
    SouthwardInitContext, SouthwardTransportMeter,
};
use std::{net::SocketAddr, sync::Arc};

/// CJ/T 188 connector built from `SouthwardInitContext` (no I/O).
#[derive(Clone)]
pub struct Cjt188Connector {
    channel: Arc<Cjt188Channel>,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
    handle: Arc<Cjt188Handle>,
}

impl Cjt188Connector {
    #[inline]
    fn build_session_cfg(&self) -> SessionConfig {
        SessionConfig::new(self.channel.config.wakeup_preamble.clone())
    }

    async fn connect_once(&self, ctx: &SessionContext) -> DriverResult<Arc<dyn ProtoSession>> {
        let version = self.channel.config.version;
        match &self.channel.config.connection {
            Cjt188Connection::Serial {
                port,
                baud_rate,
                data_bits,
                stop_bits,
                parity,
            } => {
                let io = connect_serial_metered(
                    SerialConnectConfig {
                        port: port.to_string(),
                        baud_rate: *baud_rate,
                        data_bits: (*data_bits).into(),
                        stop_bits: (*stop_bits).into(),
                        parity: (*parity).into(),
                    },
                    Arc::clone(&self.transport_meter),
                )
                .map_err(|e| DriverError::SessionError(e.to_string()))?;
                Ok(Arc::new(Cjt188SessionImpl::new(
                    io,
                    self.build_session_cfg(),
                    version,
                )))
            }
            Cjt188Connection::Tcp { host, port } => {
                let addr = format!("{host}:{port}")
                    .parse::<SocketAddr>()
                    .map_err(|e| {
                        DriverError::ConfigurationError(format!(
                            "Invalid CJ/T 188 TCP address {host}:{port}: {e}"
                        ))
                    })?;
                let fut = connect_tcp_metered_with_timeout(
                    addr,
                    Arc::clone(&self.transport_meter),
                    self.channel.connection_policy.connect_timeout_ms,
                );
                let io = tokio::select! {
                    _ = ctx.cancel.cancelled() => {
                        return Err(DriverError::ServiceUnavailable);
                    }
                    res = fut => res.map_err(|e| DriverError::SessionError(format!("TCP connect failed: {e}")))?,
                };
                Ok(Arc::new(Cjt188SessionImpl::new(
                    io,
                    self.build_session_cfg(),
                    version,
                )))
            }
        }
    }
}

#[async_trait::async_trait]
impl Connector for Cjt188Connector {
    type InitContext = SouthwardInitContext;
    type Handle = Cjt188Handle;
    type Session = Cjt188Session;

    #[inline]
    async fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        let channel = ctx
            .runtime_channel
            .downcast_arc::<Cjt188Channel>()
            .map_err(|_| {
                DriverError::ConfigurationError("Invalid Cjt188Channel runtime type".to_string())
            })?;
        let handle = Arc::new(Cjt188Handle::new(Arc::clone(&channel)));
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
        let proto = self.connect_once(&ctx).await?;
        Ok(Cjt188Session::new(Arc::clone(&self.handle), proto))
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        _err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        FailureKind::Retryable
    }
}
