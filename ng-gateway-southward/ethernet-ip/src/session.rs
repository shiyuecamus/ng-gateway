//! Ethernet/IP supervised session implementation.
//!
//! Ethernet/IP does not require a separate background event loop; the client is
//! used directly for read/write operations under a mutex. We publish the connected
//! client to the data-plane handle in `Session::init()` and wait for cancellation in `run()`.

use super::handle::EthernetIpHandle;
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use rust_ethernet_ip::EipClient;
use std::sync::Arc;

/// Ethernet/IP attempt session.
pub struct EthernetIpSession {
    handle: Arc<EthernetIpHandle>,
    client: Option<EipClient>,
}

impl EthernetIpSession {
    #[inline]
    pub fn new(handle: Arc<EthernetIpHandle>, client: EipClient) -> Self {
        Self {
            handle,
            client: Some(client),
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
        if let Some(client) = self.client.take() {
            self.handle.attach_client(client);
        }
        Ok(())
    }

    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        ctx.cancel.cancelled().await;
        self.handle.detach_client();
        Ok(RunOutcome::Disconnected)
    }
}
