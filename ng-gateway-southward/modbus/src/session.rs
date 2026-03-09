//! Modbus supervised session implementation.
//!
//! Modbus has no protocol-level handshake beyond establishing the transport and creating
//! a `tokio-modbus` context. We treat the session as ready immediately after `connect()`
//! succeeds and publish the context pool to the data-plane handle in `Session::init()`.

use super::handle::{ModbusHandle, SessionPool};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use std::{sync::Arc, time::Duration};

/// Modbus attempt session.
pub struct ModbusSession {
    handle: Arc<ModbusHandle>,
    pool: Arc<SessionPool>,
}

impl ModbusSession {
    /// Create a new attempt session from a connected pool.
    #[inline]
    pub fn new(handle: Arc<ModbusHandle>, pool: Arc<SessionPool>) -> Self {
        Self { handle, pool }
    }
}

#[async_trait::async_trait]
impl Session for ModbusSession {
    type Handle = ModbusHandle;
    type Error = DriverError;

    #[inline]
    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        self.handle.set_reconnect(ctx.reconnect.clone());
        self.handle.attach_pool(Arc::clone(&self.pool));
        Ok(())
    }

    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        ctx.cancel.cancelled().await;
        if let Some(pool) = self.handle.detach_pool() {
            pool.disconnect_all(Duration::from_secs(2)).await;
        }
        Ok(RunOutcome::Disconnected)
    }
}
