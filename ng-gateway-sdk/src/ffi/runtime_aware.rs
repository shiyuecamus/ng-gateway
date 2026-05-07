//! Runtime-aware wrappers for dynamic driver/plugin libraries.
//!
//! The host loads a `cdylib` and calls `create_*_factory()` to obtain a trait
//! object. The host then drives that trait object **on the host runtime**,
//! while everything inside the `cdylib` (including its construction futures
//! and any spawned background work) MUST run on the plugin's own
//! statically-linked tokio runtime (`NG_RUNTIME`).
//!
//! These wrappers solve three orthogonal problems at the FFI seam:
//!
//! 1.  **Runtime hop for `async fn` factories** —
//!     The host calls `factory.create_*(ctx).await`. We spawn the inner
//!     future onto the plugin's `NG_RUNTIME` via `Handle::spawn(...)` and
//!     await the resulting `JoinHandle`. From the plugin author's point of
//!     view, the construction future runs in a "normal" tokio context where
//!     `tokio::spawn` / `tokio::time` / `tokio::fs` /
//!     `tokio::task::spawn_blocking` all work as expected.
//!
//! 2.  **Cancellation propagation** —
//!     The spawned `JoinHandle` is wrapped in
//!     [`tokio_util::task::AbortOnDropHandle`]. If the host-side caller
//!     drops the construction future (API timeout, request abort, parent
//!     supervision cancellation), the wrapper's `await` is dropped, the
//!     `AbortOnDropHandle` is dropped, and the inner construction task is
//!     aborted at its next `.await` point. RAII guards inside the connector
//!     reclaim partial state.
//!
//! 3.  **Data-plane isolation** —
//!     Once construction succeeds, the wrapper installs a runtime-bound
//!     actor (`RuntimeAwareDriver` / `RuntimeAwarePlugin`) that owns a
//!     bounded mailbox. All `Driver` / `Plugin` calls from the host are
//!     forwarded across the mailbox into the plugin runtime, so the
//!     `cdylib` never executes user code on a host worker.

use crate::{
    export::serde_json, northward::PluginFactory, southward::DriverFactory, CollectItem, Driver,
    DriverError, DriverResult, ExecuteResult, NGValue, NorthwardData, NorthwardError,
    NorthwardInitContext, NorthwardResult, Plugin, PluginConfig, RuntimeAction, RuntimeChannel,
    RuntimeDelta, RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardInitContext, WriteResult,
};
use std::sync::{Arc, Mutex};
use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot, Semaphore},
};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};
use tracing::{debug, info_span, warn, Instrument};

// --------------------------------------------------------------------------
// Southward driver factory wrapper
// --------------------------------------------------------------------------

/// Host-side wrapper factory for southward drivers.
///
/// # Design goals
/// - Keep `cdylib` operations (construction + data plane) on the
///   `cdylib`'s own Tokio runtime.
/// - Propagate host-side cancellation into in-flight construction work.
/// - Provide a stable, non-panicking failure mode when the runtime is
///   unavailable.
pub struct RuntimeAwareDriverFactory {
    /// The inner, plugin-defined factory. Held as `Arc` so we can clone it
    /// into the construction task spawned on the plugin runtime without
    /// transferring ownership.
    inner: Arc<dyn DriverFactory>,
    /// Mailbox capacity for the runtime-bound driver actor.
    mailbox_capacity: usize,
    /// Handle to the plugin's own `NG_RUNTIME`. `None` only when the
    /// runtime failed to build at library load time.
    rt_handle: Option<Handle>,
}

impl RuntimeAwareDriverFactory {
    /// Create a new wrapper factory.
    #[inline]
    pub fn new(
        inner: Arc<dyn DriverFactory>,
        mailbox_capacity: usize,
        rt_handle: Option<Handle>,
    ) -> Self {
        Self {
            inner,
            mailbox_capacity,
            rt_handle,
        }
    }
}

#[async_trait::async_trait]
impl DriverFactory for RuntimeAwareDriverFactory {
    async fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        let handle = self
            .rt_handle
            .clone()
            .ok_or_else(|| DriverError::ExecutionError("Driver runtime not available".into()))?;
        let inner = Arc::clone(&self.inner);
        let channel_id = ctx.channel_id;

        // Stable, traceable span around the cross-runtime construction.
        // Use i64 so log bridge layers can capture reliably.
        let span = info_span!("driver-create", channel_id = i64::from(channel_id));

        // CRITICAL: spawn onto driver's NG_RUNTIME so the inner future
        //  - sees the driver's tokio TLS for `tokio::spawn` etc.,
        //  - is cancellable via AbortOnDropHandle (caller drop → task abort).
        let join = handle.spawn(async move { inner.create_driver(ctx).await }.instrument(span));
        let abortable = AbortOnDropHandle::new(join);

        let inner_driver = match abortable.await {
            Ok(Ok(driver)) => driver,
            Ok(Err(e)) => return Err(e),
            Err(join_err) => {
                // `JoinError` here means the construction task was either
                // cancelled (host dropped the future / runtime shutting
                // down) or panicked. We surface a stable wire reason and
                // let the caller decide how to react.
                let reason = if join_err.is_cancelled() {
                    "driver construction cancelled".to_string()
                } else if join_err.is_panic() {
                    format!("driver construction panicked: {join_err}")
                } else {
                    format!("driver construction task failed: {join_err}")
                };
                return Err(DriverError::ExecutionError(reason));
            }
        };

        Ok(Box::new(RuntimeAwareDriver::new(
            inner_driver,
            channel_id,
            self.mailbox_capacity,
            self.rt_handle.clone(),
        )))
    }

    fn convert_runtime_channel(
        &self,
        channel: crate::ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>> {
        self.inner.convert_runtime_channel(channel)
    }

    fn convert_runtime_device(
        &self,
        device: crate::DeviceModel,
    ) -> DriverResult<Arc<dyn RuntimeDevice>> {
        self.inner.convert_runtime_device(device)
    }

    fn convert_runtime_point(
        &self,
        point: crate::PointModel,
    ) -> DriverResult<Arc<dyn RuntimePoint>> {
        self.inner.convert_runtime_point(point)
    }

    fn convert_runtime_action(
        &self,
        action: crate::ActionModel,
    ) -> DriverResult<Arc<dyn RuntimeAction>> {
        self.inner.convert_runtime_action(action)
    }
}

/// Control-plane messages used by the runtime-aware driver actor.
enum DriverMessage {
    Collect {
        items: Arc<[CollectItem]>,
        reply: oneshot::Sender<DriverResult<Vec<NorthwardData>>>,
    },
    Execute {
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
        reply: oneshot::Sender<DriverResult<ExecuteResult>>,
    },
    Write {
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: NGValue,
        timeout_ms: Option<u64>,
        reply: oneshot::Sender<DriverResult<WriteResult>>,
    },
    ApplyDelta {
        delta: RuntimeDelta,
        reply: oneshot::Sender<DriverResult<()>>,
    },
}

/// A runtime-bound southward driver wrapper.
///
/// This isolates the `cdylib` driver execution on the `cdylib` runtime and
/// keeps host calls lightweight.
struct RuntimeAwareDriver {
    inner: Arc<Box<dyn Driver>>,
    tx: mpsc::Sender<DriverMessage>,
    cancel_token: CancellationToken,
    channel_id: i32,
    collect_sem: Arc<Semaphore>,
    rx: Mutex<Option<mpsc::Receiver<DriverMessage>>>,
    rt_handle: Option<Handle>,
}

impl RuntimeAwareDriver {
    #[inline]
    fn new(
        inner: Box<dyn Driver>,
        channel_id: i32,
        mailbox_capacity: usize,
        rt_handle: Option<Handle>,
    ) -> Self {
        // Bounded actor mailbox. Backpressure is applied at the API boundary.
        let (tx, rx) = mpsc::channel(mailbox_capacity);
        Self {
            inner: Arc::new(inner),
            tx,
            cancel_token: CancellationToken::new(),
            channel_id,
            collect_sem: Arc::new(Semaphore::new(1)),
            rx: Mutex::new(Some(rx)),
            rt_handle,
        }
    }

    #[inline]
    fn take_rx(&self) -> DriverResult<mpsc::Receiver<DriverMessage>> {
        let mut guard = self.rx.lock().map_err(|_| {
            DriverError::ExecutionError("Driver runtime mutex poisoned".to_string())
        })?;
        guard.take().ok_or(DriverError::ExecutionError(
            "Driver already started".to_string(),
        ))
    }
}

#[async_trait::async_trait]
impl Driver for RuntimeAwareDriver {
    async fn start(&self) -> DriverResult<()> {
        let handle = self.rt_handle.clone().ok_or(DriverError::ExecutionError(
            "Driver runtime not available".to_string(),
        ))?;

        let inner = Arc::clone(&self.inner);
        let cancel_token = self.cancel_token.clone();
        let collect_sem = Arc::clone(&self.collect_sem);
        let channel_id = self.channel_id;
        let mut rx = self.take_rx()?;

        let (tx_res, rx_res) = oneshot::channel();

        // Use i64 so log bridge layers can capture reliably.
        let actor_span = info_span!("driver-actor", channel_id = i64::from(channel_id));
        handle.spawn(async move {
            let start_span = info_span!("driver-start", channel_id = i64::from(channel_id));
            let inner_start = Arc::clone(&inner);
            if let Err(e) = async move { inner_start.start().await }
                .instrument(start_span)
                .await
            {
                let _ = tx_res.send(Err(e));
                return;
            }
            let _ = tx_res.send(Ok(()));

            // Initialize collect concurrency after start.
            let profile = inner.collector_concurrency_profile();
            let collect_max = profile.get();
            if collect_max > 1 {
                collect_sem.add_permits(collect_max.saturating_sub(1));
            }

            debug!(
                "Driver actor loop started, collect profile: {:?}",
                profile
            );

            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        break;
                    }
                    maybe_msg = rx.recv() => {
                        let Some(msg) = maybe_msg else { break; };
                        let inner = Arc::clone(&inner);
                        let collect_sem = Arc::clone(&collect_sem);
                        // Preserve current span (contains `channel_id`) for per-message execution.
                        tokio::spawn(async move {
                            match msg {
                                DriverMessage::Collect { items, reply } => {
                                    // Bound collect inflight further.
                                    let _permit = collect_sem.acquire_owned().await;
                                    let res = inner.collect_data(items.as_ref()).await;
                                    let _ = reply.send(res);
                                }
                                DriverMessage::Execute { device, action, parameters, reply } => {
                                    let res = inner.execute_action(device, action, parameters).await;
                                    let _ = reply.send(res);
                                }
                                DriverMessage::Write { device, point, value, timeout_ms, reply } => {
                                    let res = inner.write_point(device, point, &value, timeout_ms).await;
                                    let _ = reply.send(res);
                                }
                                DriverMessage::ApplyDelta { delta, reply } => {
                                    let res = inner.apply_runtime_delta(delta).await;
                                    let _ = reply.send(res);
                                }
                            }
                        }
                        .in_current_span());
                    }
                }
            }

            debug!("Driver actor loop stopped");
            let _ = inner.stop().await;
        }
        .instrument(actor_span));

        match rx_res.await {
            Ok(result) => result,
            Err(_) => Err(DriverError::ExecutionError(
                "Driver start task cancelled".to_string(),
            )),
        }
    }

    async fn stop(&self) -> DriverResult<()> {
        self.cancel_token.cancel();
        Ok(())
    }

    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        let (tx, rx) = oneshot::channel();
        let items: Arc<[CollectItem]> = Arc::from(items);
        self.tx
            .send(DriverMessage::Collect { items, reply: tx })
            .await
            .map_err(|_| DriverError::Unreachable("Driver mailbox closed".to_string()))?;
        rx.await
            .map_err(|_| DriverError::ExecutionError("Driver collect cancelled".to_string()))?
    }

    #[inline]
    fn collection_group_key(
        &self,
        device: &dyn RuntimeDevice,
    ) -> Option<crate::CollectionGroupKey> {
        self.inner.collection_group_key(device)
    }

    #[inline]
    fn collector_concurrency_profile(&self) -> crate::CollectorConcurrencyProfile {
        self.inner.collector_concurrency_profile()
    }

    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::Execute {
                device,
                action,
                parameters,
                reply: tx,
            })
            .await
            .map_err(|_| DriverError::Unreachable("Driver mailbox closed".to_string()))?;
        rx.await
            .map_err(|_| DriverError::ExecutionError("Driver execute cancelled".to_string()))?
    }

    async fn write_point(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let value = value.clone();
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::Write {
                device,
                point,
                value,
                timeout_ms,
                reply: tx,
            })
            .await
            .map_err(|_| DriverError::Unreachable("Driver mailbox closed".to_string()))?;
        rx.await
            .map_err(|_| DriverError::ExecutionError("Driver write cancelled".to_string()))?
    }

    async fn apply_runtime_delta(&self, delta: RuntimeDelta) -> DriverResult<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::ApplyDelta { delta, reply: tx })
            .await
            .map_err(|_| DriverError::Unreachable("Driver mailbox closed".to_string()))?;
        rx.await.map_err(|_| {
            DriverError::ExecutionError("Driver apply_runtime_delta cancelled".to_string())
        })?
    }

    fn subscribe_connection_state(
        &self,
    ) -> tokio::sync::watch::Receiver<Arc<crate::ConnectionState>> {
        self.inner.subscribe_connection_state()
    }
}

// --------------------------------------------------------------------------
// Northward plugin factory wrapper
// --------------------------------------------------------------------------

/// Host-side wrapper factory for northward plugins.
///
/// See the module-level documentation for the design contract.
pub struct RuntimeAwarePluginFactory {
    /// The inner, plugin-defined factory. Held as `Arc` so we can clone it
    /// into the construction task spawned on the plugin runtime.
    inner: Arc<dyn PluginFactory>,
    channel_capacity: usize,
    rt_handle: Option<Handle>,
}

impl RuntimeAwarePluginFactory {
    #[inline]
    pub fn new(
        inner: Arc<dyn PluginFactory>,
        channel_capacity: usize,
        rt_handle: Option<Handle>,
    ) -> Self {
        Self {
            inner,
            channel_capacity,
            rt_handle,
        }
    }
}

#[async_trait::async_trait]
impl PluginFactory for RuntimeAwarePluginFactory {
    async fn create_plugin(&self, ctx: NorthwardInitContext) -> NorthwardResult<Box<dyn Plugin>> {
        let handle = self.rt_handle.clone().ok_or(NorthwardError::RuntimeError {
            reason: "Plugin runtime not available".to_string(),
        })?;
        let inner = Arc::clone(&self.inner);
        let app_id = ctx.app_id;

        // Stable, traceable span around the cross-runtime construction.
        let span = info_span!("plugin-create", app_id = i64::from(app_id));

        // CRITICAL: spawn onto plugin's NG_RUNTIME so the inner future
        //  - sees the plugin's tokio TLS for `tokio::spawn` etc.,
        //  - is cancellable via AbortOnDropHandle (caller drop → task abort).
        let join = handle.spawn(async move { inner.create_plugin(ctx).await }.instrument(span));
        let abortable = AbortOnDropHandle::new(join);

        let inner_plugin = match abortable.await {
            Ok(Ok(plugin)) => plugin,
            Ok(Err(e)) => return Err(e),
            Err(join_err) => {
                // `JoinError` here means the construction task was either
                // cancelled (host dropped the future / runtime shutting
                // down) or panicked. We surface a stable wire reason and
                // let the caller decide how to react.
                let reason = if join_err.is_cancelled() {
                    "plugin construction cancelled".to_string()
                } else if join_err.is_panic() {
                    format!("plugin construction panicked: {join_err}")
                } else {
                    format!("plugin construction task failed: {join_err}")
                };
                return Err(NorthwardError::RuntimeError { reason });
            }
        };

        let (tx, rx) = mpsc::channel(self.channel_capacity);
        Ok(Box::new(RuntimeAwarePlugin {
            inner: Arc::new(inner_plugin),
            app_id,
            tx,
            cancel_token: CancellationToken::new(),
            rx: Mutex::new(Some(rx)),
            rt_handle: self.rt_handle.clone(),
        }))
    }

    fn convert_plugin_config(
        &self,
        config: serde_json::Value,
    ) -> NorthwardResult<Arc<dyn PluginConfig>> {
        self.inner.convert_plugin_config(config)
    }
}

struct RuntimeAwarePlugin {
    inner: Arc<Box<dyn Plugin>>,
    app_id: i32,
    tx: mpsc::Sender<Arc<NorthwardData>>,
    cancel_token: CancellationToken,
    rx: Mutex<Option<mpsc::Receiver<Arc<NorthwardData>>>>,
    rt_handle: Option<Handle>,
}

#[async_trait::async_trait]
impl Plugin for RuntimeAwarePlugin {
    async fn start(&self) -> NorthwardResult<()> {
        let handle = self.rt_handle.clone().ok_or(NorthwardError::RuntimeError {
            reason: "Plugin runtime not available".to_string(),
        })?;

        let inner = Arc::clone(&self.inner);
        let cancel_token = self.cancel_token.clone();
        let app_id = self.app_id;
        let mut rx = {
            let mut guard = self.rx.lock().map_err(|_| NorthwardError::RuntimeError {
                reason: "Plugin runtime mutex poisoned".to_string(),
            })?;
            guard.take().ok_or(NorthwardError::RuntimeError {
                reason: "Plugin already started".to_string(),
            })?
        };

        let (tx_res, rx_res) = oneshot::channel();

        // Use i64 so log bridge layers can capture reliably.
        let actor_span = info_span!("plugin-actor", app_id = i64::from(app_id));
        handle.spawn(
            async move {
                if let Err(e) = inner.start().await {
                    let _ = tx_res.send(Err(e));
                    return;
                }
                let _ = tx_res.send(Ok(()));

                debug!("Plugin actor loop started");
                loop {
                    tokio::select! {
                        _ = cancel_token.cancelled() => break,
                        maybe_msg = rx.recv() => {
                            match maybe_msg {
                                Some(data) => {
                                    if let Err(e) = inner.process_data(data).await {
                                        warn!("Error processing northward data: {}", e);
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                }
                debug!("Plugin actor loop stopped");
                let _ = inner.stop().await;
            }
            .instrument(actor_span),
        );

        match rx_res.await {
            Ok(result) => result,
            Err(_) => Err(NorthwardError::RuntimeError {
                reason: "Plugin start task cancelled".to_string(),
            }),
        }
    }

    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        self.tx
            .send(data)
            .await
            .map_err(|_| NorthwardError::NotConnected)
    }

    fn subscribe_connection_state(
        &self,
    ) -> tokio::sync::watch::Receiver<Arc<crate::ConnectionState>> {
        self.inner.subscribe_connection_state()
    }

    async fn invoke_capability(
        &self,
        capability_id: &str,
        request: serde_json::Value,
    ) -> NorthwardResult<serde_json::Value> {
        self.inner.invoke_capability(capability_id, request).await
    }

    async fn stop(&self) -> NorthwardResult<()> {
        self.cancel_token.cancel();
        Ok(())
    }
}
