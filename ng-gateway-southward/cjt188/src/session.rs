//! CJ/T 188 supervised session implementation.
//!
//! CJ/T 188 is request/response over an already-connected transport. There is no
//! separate handshake actor like S7, so we treat the session as Ready immediately
//! after `Connector::connect()` succeeds.
//!
//! Reconnect requests are issued from the data-plane (`Cjt188Handle`) via the
//! injected `ReconnectHandle`, and the supervisor loop will cancel the attempt.

use super::{handle::Cjt188Handle, protocol::session::Cjt188Session as ProtoSession};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use std::sync::Arc;

/// CJ/T 188 attempt session.
pub struct Cjt188Session {
    handle: Arc<Cjt188Handle>,
    proto_session: Arc<dyn ProtoSession>,
}

impl Cjt188Session {
    /// Create an attempt session from a connected protocol session.
    #[inline]
    pub fn new(handle: Arc<Cjt188Handle>, proto_session: Arc<dyn ProtoSession>) -> Self {
        Self {
            handle,
            proto_session,
        }
    }
}

#[async_trait::async_trait]
impl Session for Cjt188Session {
    type Handle = Cjt188Handle;
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
        let _ = self.proto_session.close().await;
        Ok(RunOutcome::Disconnected)
    }
}
