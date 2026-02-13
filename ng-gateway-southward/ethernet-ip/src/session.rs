//! Ethernet/IP supervised session implementation.
//!
//! Ethernet/IP does not require a separate background event loop; the client is
//! used directly for read/write operations under a mutex. We publish the connected
//! session pool to the data-plane handle in `Session::init()` and wait for
//! cancellation in `run()`.

use super::handle::{EipSessionPool, EthernetIpHandle};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use std::sync::Arc;

/// Ethernet/IP attempt session.
pub struct EthernetIpSession {
    handle: Arc<EthernetIpHandle>,
    pool: Option<Arc<EipSessionPool>>,
}

impl EthernetIpSession {
    #[inline]
    pub fn new(handle: Arc<EthernetIpHandle>, pool: Arc<EipSessionPool>) -> Self {
        Self {
            handle,
            pool: Some(pool),
        }
    }
}

#[async_trait::async_trait]
impl Session for EthernetIpSession {
    type Handle = EthernetIpHandle;
    type Error = DriverError;

    #[inline]
    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        self.handle.set_reconnect(ctx.reconnect.clone());
        if let Some(pool) = self.pool.take() {
            self.handle.attach_pool(pool);
        }
        Ok(())
    }

    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        ctx.cancel.cancelled().await;
        // Detach pool; the returned Arc is dropped here, triggering Drop on EipClient(s).
        let _old_pool = self.handle.detach_pool();
        Ok(RunOutcome::Disconnected)
    }
}
