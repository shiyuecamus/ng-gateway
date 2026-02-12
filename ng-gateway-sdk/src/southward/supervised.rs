//! Supervised southward driver wrapper.
//!
//! This module provides `SupervisedDriver`, an SDK-managed implementation of `Driver`
//! built on top of the unified supervision loop.
//!
//! # Design goals
//! - Data-plane hot path reads the handle via `HandleCell` (lock-free).
//! - Connection lifecycle is governed by `SupervisorLoop`.
//! - State subscription is the single authoritative `watch<Arc<ConnectionState>>`.

use super::super::{
    supervision::{Connector, SupervisorHandle, SupervisorLoop},
    CollectItem, CollectionGroupKey, CollectorConcurrencyProfile, ConnectionState, Driver,
    DriverError, DriverResult, ExecuteResult, NGValue, NorthwardData, RuntimeAction, RuntimeDelta,
    RuntimeDevice, RuntimeParameter, RuntimePoint, WriteResult,
};
use async_trait::async_trait;
use downcast_rs::{impl_downcast, DowncastSync};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{watch, Mutex};

/// Data-plane handle contract for supervised southward drivers.
///
/// This is the **hot-path** interface that `SupervisedDriver` delegates to when connected.
/// Implementations should keep methods efficient and avoid allocations where possible.
#[async_trait]
pub trait SouthwardHandle: DowncastSync + Send + Sync + 'static {
    /// Return collection group key (must be fast and allocation-free).
    fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        let _ = device;
        None
    }

    /// Collection concurrency capability profile for this handle.
    ///
    /// When connected, `SupervisedDriver` will prefer this value over any connector/default hint.
    /// Implementations should return a stable value and avoid allocations.
    #[inline]
    fn collector_concurrency_profile(&self) -> CollectorConcurrencyProfile {
        CollectorConcurrencyProfile::serial()
    }

    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>>;

    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult>;

    async fn write_point(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult>;

    async fn apply_runtime_delta(&self, delta: RuntimeDelta) -> DriverResult<()> {
        let _ = delta;
        Ok(())
    }
}

impl_downcast!(sync SouthwardHandle);

/// An SDK-supervised `Driver` backed by `SupervisorLoop`.
///
/// `C::Handle` is the data-plane handle published by the supervision loop.
pub struct SupervisedDriver<C>
where
    C: Connector,
    C::Handle: SouthwardHandle,
{
    supervisor: Arc<SupervisorLoop<C>>,
    started: AtomicBool,
    handle: Mutex<Option<SupervisorHandle>>,
    /// Default concurrency profile before the handle becomes available.
    ///
    /// This is used by the ABI runtime adapter and the Collector to size semaphores
    /// before the first successful connect publishes the handle.
    concurrency_profile_default: CollectorConcurrencyProfile,
}

impl<C> SupervisedDriver<C>
where
    C: Connector,
    C::Handle: SouthwardHandle,
{
    /// Create a supervised driver from an already configured supervisor loop.
    #[inline]
    pub fn new(supervisor: SupervisorLoop<C>) -> Self {
        Self::new_with_concurrency_profile(supervisor, CollectorConcurrencyProfile::serial())
    }

    /// Create a supervised driver with an explicit default concurrency profile.
    #[inline]
    pub fn new_with_concurrency_profile(
        supervisor: SupervisorLoop<C>,
        concurrency_profile_default: CollectorConcurrencyProfile,
    ) -> Self {
        Self {
            supervisor: Arc::new(supervisor),
            started: AtomicBool::new(false),
            handle: Mutex::new(None),
            concurrency_profile_default,
        }
    }

    #[inline]
    fn load_handle(&self) -> DriverResult<Arc<C::Handle>> {
        self.supervisor
            .load_handle()
            .ok_or(DriverError::ServiceUnavailable)
    }
}

#[async_trait]
impl<C> Driver for SupervisedDriver<C>
where
    C: Connector,
    C::Handle: SouthwardHandle,
{
    #[inline]
    async fn start(&self) -> DriverResult<()> {
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

    #[inline]
    async fn stop(&self) -> DriverResult<()> {
        if let Some(h) = self.handle.lock().await.take() {
            h.stop();
        }
        Ok(())
    }

    #[inline]
    fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        self.supervisor
            .load_handle()
            .and_then(|h| h.collection_group_key(device))
    }

    #[inline]
    fn collector_concurrency_profile(&self) -> CollectorConcurrencyProfile {
        self.supervisor
            .load_handle()
            .map(|h| h.collector_concurrency_profile())
            .unwrap_or(self.concurrency_profile_default)
    }

    #[inline]
    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        let h = self.load_handle()?;
        h.collect_data(items).await
    }

    #[inline]
    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let h = self.load_handle()?;
        h.execute_action(device, action, parameters).await
    }

    #[inline]
    async fn write_point(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let h = self.load_handle()?;
        h.write_point(device, point, value, timeout_ms).await
    }

    #[inline]
    fn subscribe_connection_state(&self) -> watch::Receiver<Arc<ConnectionState>> {
        self.supervisor.subscribe_state()
    }

    #[inline]
    async fn apply_runtime_delta(&self, delta: RuntimeDelta) -> DriverResult<()> {
        let h = self.load_handle()?;
        h.apply_runtime_delta(delta).await
    }
}
