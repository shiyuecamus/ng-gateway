//! DL/T 645 supervised session implementation.
//!
//! DL/T 645 does not require a background IO driver task: request/response is driven
//! by protocol session methods. We treat the session as Ready immediately after
//! the transport is established and attach it into `Dl645Handle`.

use super::{handle::Dl645Handle, protocol::session::Dl645Session as ProtoSession};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use std::sync::Arc;

/// DL/T 645 attempt session.
pub struct Dl645Session {
    handle: Arc<Dl645Handle>,
    proto_session: Arc<dyn ProtoSession>,
}

impl Dl645Session {
    /// Create an attempt session from a connected protocol session.
    #[inline]
    pub fn new(handle: Arc<Dl645Handle>, proto_session: Arc<dyn ProtoSession>) -> Self {
        Self {
            handle,
            proto_session,
        }
    }
}

#[async_trait::async_trait]
impl Session for Dl645Session {
    type Handle = Dl645Handle;
    type Error = DriverError;

    #[inline]
    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        // Inject reconnect handle for data-plane best-effort requests.
        self.handle.set_reconnect(ctx.reconnect.clone());
        // Publish the protocol session.
        self.handle.attach_session(Arc::clone(&self.proto_session));
        Ok(())
    }

    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        // The supervisor loop will cancel this attempt when reconnect is requested.
        ctx.cancel.cancelled().await;
        self.handle.detach_session();
        Ok(RunOutcome::Disconnected)
    }
}
