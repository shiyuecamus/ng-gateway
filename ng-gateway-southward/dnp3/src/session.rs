//! DNP3 supervised session implementation.
//!
//! This session owns the polling loop (integrity + event scans) and publishes the
//! live `AssociationHandle` into `Dnp3Handle` for downlink operations.
//!
//! DNP3 is primarily report-driven via SOE callbacks; polling here ensures periodic
//! integrity and event class scans for robustness.

use super::handle::Dnp3Handle;
use dnp3::{
    app::Variation,
    master::{AssociationHandle, Classes, ReadHeader, ReadRequest},
};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use std::{sync::Arc, time::Duration};

/// DNP3 attempt session.
pub struct Dnp3Session {
    handle: Arc<Dnp3Handle>,
    association: AssociationHandle,
}

impl Dnp3Session {
    pub fn new(handle: Arc<Dnp3Handle>, association: AssociationHandle) -> Self {
        Self {
            handle,
            association,
        }
    }
}

#[async_trait::async_trait]
impl Session for Dnp3Session {
    type Handle = Dnp3Handle;
    type Error = DriverError;

    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, _ctx: &SessionContext) -> Result<(), Self::Error> {
        // Publish association for downlink.
        self.handle.attach_association(self.association.clone());

        // A small "handshake" step: perform an initial class scan to ensure the association is usable.
        // If this fails, we error out so the supervisor will reconnect.
        let req = ReadRequest::class_scan(Classes::all());
        let _ = tokio::time::timeout(Duration::from_secs(5), self.association.read(req))
            .await
            .map_err(|_| DriverError::Timeout(Duration::from_secs(5)))?
            .map_err(|e| DriverError::SessionError(format!("DNP3 initial read failed: {:?}", e)))?;

        Ok(())
    }

    async fn run(mut self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        // Periodic scans.
        let mut interval_integrity = tokio::time::interval(Duration::from_millis(
            self.handle.inner.config.integrity_scan_interval_ms.max(1),
        ));
        let mut interval_event = tokio::time::interval(Duration::from_millis(
            self.handle.inner.config.event_scan_interval_ms.max(1),
        ));

        // Tick immediately (optional) to spread load predictably.
        interval_integrity.reset();
        interval_event.reset();

        // Initial best-effort integrity scan (non-fatal).
        let _ = self
            .association
            .read(ReadRequest::class_scan(Classes::all()))
            .await;

        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    self.handle.detach_association();
                    return Ok(RunOutcome::Disconnected);
                }
                _ = interval_integrity.tick() => {
                    let headers = vec![
                        ReadHeader::all_objects(Variation::Group60Var1),
                        ReadHeader::all_objects(Variation::Group60Var2),
                        ReadHeader::all_objects(Variation::Group60Var3),
                        ReadHeader::all_objects(Variation::Group60Var4),
                        ReadHeader::all_objects(Variation::Group110(0)),
                    ];
                    let req = ReadRequest::multiple_headers(&headers);
                    if let Err(e) = self.association.read(req).await {
                        // Integrity scan is best-effort; keep running.
                        tracing::warn!("DNP3 integrity scan failed: {:?}", e);
                    }
                }
                _ = interval_event.tick() => {
                    // Event scan failures mean the association is unhealthy.
                    if let Err(_e) = self.association.read(ReadRequest::class_scan(Classes::class123())).await {
                        self.handle.detach_association();
                        return Ok(RunOutcome::ReconnectRequested(Arc::<str>::from("dnp3 event scan failed")));
                    }
                }
            }
        }
    }
}
