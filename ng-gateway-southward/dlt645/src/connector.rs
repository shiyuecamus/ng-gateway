//! DL/T 645 supervision connector.
//!
//! Final-form integration:
//! - `Dl645Connector`: implements SDK `Connector` and is constructed via `Connector::new(ctx)`.
//! - `connect()`: establishes transport and returns an attempt-scoped `Dl645Session`.

use super::{
    handle::Dl645Handle,
    protocol::session::{Dl645Session as ProtoSession, Dl645SessionImpl, SessionConfig},
    session::Dl645Session,
    types::{Dl645Channel, Dl645Connection},
};
use ng_gateway_sdk::{
    connect_serial_metered, connect_tcp_metered_with_timeout,
    supervision::{Connector, Session, SessionContext},
    DriverError, DriverResult, FailureKind, FailurePhase, SerialConnectConfig,
    SouthwardInitContext, SouthwardTransportMeter,
};
use std::{net::SocketAddr, sync::Arc};

/// DL/T 645 connector built from `SouthwardInitContext` (no I/O).
#[derive(Clone)]
pub struct Dl645Connector {
    channel: Arc<Dl645Channel>,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
    handle: Arc<Dl645Handle>,
}

impl Dl645Connector {
    #[inline]
    fn build_session_cfg(&self) -> SessionConfig {
        SessionConfig::new(
            self.channel.config.wakeup_preamble.clone(),
            self.channel.config.version,
        )
    }

    async fn connect_once(&self, ctx: &SessionContext) -> DriverResult<Arc<dyn ProtoSession>> {
        match &self.channel.config.connection {
            Dl645Connection::Serial {
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
                Ok(Arc::new(Dl645SessionImpl::new(
                    io,
                    self.build_session_cfg(),
                )))
            }
            Dl645Connection::Tcp { host, port } => {
                let addr = format!("{host}:{port}")
                    .parse::<SocketAddr>()
                    .map_err(|e| {
                        DriverError::ConfigurationError(format!(
                            "Invalid DL/T 645 TCP address {host}:{port}: {e}"
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
                Ok(Arc::new(Dl645SessionImpl::new(
                    io,
                    self.build_session_cfg(),
                )))
            }
        }
    }
}

#[async_trait::async_trait]
impl Connector for Dl645Connector {
    type InitContext = SouthwardInitContext;
    type Handle = Dl645Handle;
    type Session = Dl645Session;

    #[inline]
    async fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        let channel = ctx
            .runtime_channel
            .downcast_arc::<Dl645Channel>()
            .map_err(|_| {
                DriverError::ConfigurationError("Invalid Dl645Channel runtime type".to_string())
            })?;
        let handle = Arc::new(Dl645Handle::new(Arc::clone(&channel)));
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
        Ok(Dl645Session::new(Arc::clone(&self.handle), proto))
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        _err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        FailureKind::Retryable
    }
}
