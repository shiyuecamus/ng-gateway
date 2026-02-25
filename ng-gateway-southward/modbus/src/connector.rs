//! Modbus supervision connector.
//!
//! Final-form integration:
//! - `ModbusConnector`: implements SDK `Connector` and is constructed via `Connector::new(ctx)`.
//! - `connect()`: establishes transport and creates a context pool, returning `ModbusSession`.

use super::{
    handle::{ModbusHandle, SessionPool},
    session::ModbusSession,
    types::{ModbusChannel, ModbusChannelConfig, ModbusConnection},
};
use ng_gateway_sdk::{
    connect_serial_metered, connect_tcp_metered_with_timeout,
    supervision::{Connector, Session, SessionContext},
    CollectorConcurrencyProfile, DriverError, DriverResult, FailureKind, FailurePhase,
    NoopSouthwardTransportMeter, SerialConnectConfig, SouthwardInitContext,
    SouthwardTransportMeter,
};
use std::{net::SocketAddr, sync::Arc};
use tokio_modbus::client::{rtu, tcp};

/// Modbus connector built from `SouthwardInitContext` (no I/O).
#[derive(Clone)]
pub struct ModbusConnector {
    channel: Arc<ModbusChannel>,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
    handle: Arc<ModbusHandle>,
}

impl ModbusConnector {
    #[inline]
    async fn connect_pool(
        &self,
        ctx: &SessionContext,
        cfg: &ModbusChannelConfig,
    ) -> DriverResult<Arc<SessionPool>> {
        match &cfg.connection {
            ModbusConnection::Tcp { host, port } => {
                let addr = format!("{host}:{port}")
                    .parse::<SocketAddr>()
                    .map_err(|e| {
                        DriverError::ConfigurationError(format!("Invalid socket address: {e}"))
                    })?;
                let size = cfg.tcp_pool_size.clamp(1, 32) as usize;
                let mut contexts = Vec::with_capacity(size);
                for _ in 0..size {
                    let fut = connect_tcp_metered_with_timeout(
                        addr,
                        Arc::clone(&self.transport_meter),
                        self.channel.connection_policy.connect_timeout_ms,
                    );
                    let stream = tokio::select! {
                        _ = ctx.cancel.cancelled() => {
                            return Err(DriverError::ServiceUnavailable);
                        }
                        res = fut => res.map_err(|e| DriverError::SessionError(format!("Modbus TCP connect error: {e}")))?,
                    };
                    contexts.push(tcp::attach(stream));
                }
                Ok(Arc::new(SessionPool::new(contexts)))
            }
            ModbusConnection::Rtu {
                port,
                baud_rate,
                data_bits,
                stop_bits,
                parity,
            } => {
                if ctx.cancel.is_cancelled() {
                    return Err(DriverError::ServiceUnavailable);
                }
                let stream = connect_serial_metered(
                    SerialConnectConfig {
                        port: port.to_string(),
                        baud_rate: *baud_rate,
                        data_bits: (*data_bits).into(),
                        stop_bits: (*stop_bits).into(),
                        parity: (*parity).into(),
                    },
                    Arc::clone(&self.transport_meter),
                )
                .map_err(|e| {
                    DriverError::SessionError(format!("Failed to open serial port {port}: {e}"))
                })?;
                Ok(Arc::new(SessionPool::new(vec![rtu::attach(stream)])))
            }
        }
    }
}

#[async_trait::async_trait]
impl Connector for ModbusConnector {
    type InitContext = SouthwardInitContext;
    type Handle = ModbusHandle;
    type Session = ModbusSession;

    fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        let transport_meter = ctx
            .extensions
            .get_or_default(|| Arc::new(NoopSouthwardTransportMeter) as Arc<dyn SouthwardTransportMeter>);
        let channel = ctx
            .runtime_channel
            .downcast_arc::<ModbusChannel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid ModbusChannel".to_string()))?;
        let handle = Arc::new(ModbusHandle::new(Arc::clone(&channel)));
        Ok(Self {
            channel,
            transport_meter,
            handle,
        })
    }

    #[inline]
    fn collector_concurrency_profile_hint(&self) -> CollectorConcurrencyProfile {
        let n = match &self.channel.config.connection {
            ModbusConnection::Tcp { .. } => self.channel.config.tcp_pool_size.clamp(1, 32) as usize,
            ModbusConnection::Rtu { .. } => 1usize,
        };
        CollectorConcurrencyProfile::concurrent(n)
    }

    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let pool = self.connect_pool(&ctx, &self.channel.config).await?;
        Ok(ModbusSession::new(Arc::clone(&self.handle), pool))
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        _err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        FailureKind::Retryable
    }
}
