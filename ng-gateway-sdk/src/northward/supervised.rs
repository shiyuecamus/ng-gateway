//! Supervised northward plugin wrapper.
//!
//! This module provides `SupervisedPlugin`, an SDK-managed implementation of `Plugin`
//! built on top of the unified supervision loop.
//!
//! # Design goals
//! - Data-plane hot path reads the handle via `HandleCell` (lock-free).
//! - Connection lifecycle is governed by `SupervisorLoop`.
//! - State subscription is the single authoritative `watch<Arc<ConnectionState>>`.

use crate::{
    supervision::{Connector, SupervisorHandle, SupervisorLoop},
    ConnectionState, NorthwardData, NorthwardError, NorthwardResult, Plugin,
};
use async_trait::async_trait;
use downcast_rs::{impl_downcast, DowncastSync};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{watch, Mutex};

/// Data-plane handle contract for supervised northward plugins.
#[async_trait]
pub trait NorthwardHandle: DowncastSync + Send + Sync + 'static {
    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()>;

    async fn ping(&self) -> NorthwardResult<Duration> {
        Err(NorthwardError::NotConnected)
    }
}

impl_downcast!(sync NorthwardHandle);

/// An SDK-supervised `Plugin` backed by `SupervisorLoop`.
pub struct SupervisedPlugin<C>
where
    C: Connector,
    C::Handle: NorthwardHandle,
{
    supervisor: Arc<SupervisorLoop<C>>,
    started: AtomicBool,
    handle: Mutex<Option<SupervisorHandle>>,
}

impl<C> SupervisedPlugin<C>
where
    C: Connector,
    C::Handle: NorthwardHandle,
{
    /// Create a supervised plugin from an already configured supervisor loop.
    #[inline]
    pub fn new(supervisor: SupervisorLoop<C>) -> Self {
        Self {
            supervisor: Arc::new(supervisor),
            started: AtomicBool::new(false),
            handle: Mutex::new(None),
        }
    }

    #[inline]
    fn load_handle(&self) -> NorthwardResult<Arc<C::Handle>> {
        self.supervisor
            .load_handle()
            .ok_or(NorthwardError::NotConnected)
    }
}

#[async_trait]
impl<C> Plugin for SupervisedPlugin<C>
where
    C: Connector,
    C::Handle: NorthwardHandle,
{
    async fn start(&self) -> NorthwardResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let h = Arc::clone(&self.supervisor).start();
        *self.handle.lock().await = Some(h);
        Ok(())
    }

    fn subscribe_connection_state(&self) -> watch::Receiver<Arc<ConnectionState>> {
        self.supervisor.subscribe_state()
    }

    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        let h = self.load_handle()?;
        h.process_data(data).await
    }

    async fn invoke_capability(
        &self,
        capability_id: &str,
        request: serde_json::Value,
    ) -> NorthwardResult<serde_json::Value> {
        self.supervisor
            .invoke_capability(capability_id, request)
            .await
    }

    async fn stop(&self) -> NorthwardResult<()> {
        if let Some(h) = self.handle.lock().await.take() {
            h.stop();
        }
        Ok(())
    }
}
