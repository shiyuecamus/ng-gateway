//! Runtime-aware wrappers for dynamic driver/plugin libraries.
//!
//! The host loads a `cdylib` and calls `create_*_factory()` to obtain a trait object.
//! The returned factory must:
//! - avoid leaking the host runtime into the `cdylib` boundary
//! - provide bounded queues / backpressure
//! - ensure the data-path does not block the host
//!
//! These wrappers are instantiated by macros to keep macro bodies clean.

use crate::{
    export::serde_json, northward::PluginFactory, southward::DriverFactory, CollectItem, Driver,
    DriverError, DriverResult, ExecuteResult, NGValue, NorthwardData, NorthwardError,
    NorthwardInitContext, NorthwardResult, Plugin, PluginConfig, RuntimeAction, RuntimeChannel,
    RuntimeDelta, RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardInitContext, WriteResult,
};
use std::sync::Arc;
use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot, Semaphore},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info_span, warn, Instrument};

/// Host-side wrapper factory for southward drivers.
///
/// # Design goals
/// - Keep `cdylib` operations on the `cdylib`'s own Tokio runtime (if available).
/// - Provide a stable, non-panicking failure mode if the runtime is unavailable.
pub struct RuntimeAwareDriverFactory {
    inner: Box<dyn DriverFactory>,
    mailbox_capacity: usize,
    rt_handle: Option<Handle>,
}

impl RuntimeAwareDriverFactory {
    /// Create a new wrapper factory.
    #[inline]
    pub fn new(
        inner: Box<dyn DriverFactory>,
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

impl DriverFactory for RuntimeAwareDriverFactory {
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
        let channel_id = ctx.channel_id;
        let inner_driver = self.inner.create_driver(ctx)?;
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
/// This isolates the `cdylib` driver execution on the `cdylib` runtime and keeps
/// host calls lightweight.
struct RuntimeAwareDriver {
    inner: Arc<Box<dyn Driver>>,
    tx: mpsc::Sender<DriverMessage>,
    cancel_token: CancellationToken,
    channel_id: i32,
    collect_sem: Arc<Semaphore>,
    rx: std::sync::Mutex<Option<mpsc::Receiver<DriverMessage>>>,
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
            rx: std::sync::Mutex::new(Some(rx)),
            rt_handle,
        }
    }

    #[inline]
    fn take_rx(&self) -> DriverResult<mpsc::Receiver<DriverMessage>> {
        let mut guard = self.rx.lock().map_err(|_| {
            DriverError::ExecutionError("Driver runtime mutex poisoned".to_string())
        })?;
        guard
            .take()
            .ok_or_else(|| DriverError::ExecutionError("Driver already started".to_string()))
    }
}

#[async_trait::async_trait]
impl Driver for RuntimeAwareDriver {
    async fn start(&self) -> DriverResult<()> {
        let handle = self.rt_handle.clone().ok_or_else(|| {
            DriverError::ExecutionError("Driver runtime not available".to_string())
        })?;

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

        rx_res.await.unwrap_or(Err(DriverError::ExecutionError(
            "Driver start task cancelled".to_string(),
        )))
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
            .map_err(|_| DriverError::ExecutionError("Driver mailbox closed".to_string()))?;
        rx.await
            .map_err(|_| DriverError::ExecutionError("Driver collect cancelled".to_string()))?
    }

    #[inline]
    fn collector_concurrency_profile(&self) -> crate::CollectorConcurrencyProfile {
        // Forward the inner driver's capability profile to the host.
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
            .map_err(|_| DriverError::ExecutionError("Driver mailbox closed".to_string()))?;
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
            .map_err(|_| DriverError::ExecutionError("Driver mailbox closed".to_string()))?;
        rx.await
            .map_err(|_| DriverError::ExecutionError("Driver write cancelled".to_string()))?
    }

    async fn apply_runtime_delta(&self, delta: RuntimeDelta) -> DriverResult<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(DriverMessage::ApplyDelta { delta, reply: tx })
            .await
            .map_err(|_| DriverError::ExecutionError("Driver mailbox closed".to_string()))?;
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

/// Host-side wrapper factory for northward plugins.
pub struct RuntimeAwarePluginFactory {
    inner: Box<dyn PluginFactory>,
    channel_capacity: usize,
    rt_handle: Option<Handle>,
}

impl RuntimeAwarePluginFactory {
    #[inline]
    pub fn new(
        inner: Box<dyn PluginFactory>,
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

impl PluginFactory for RuntimeAwarePluginFactory {
    fn create_plugin(&self, ctx: NorthwardInitContext) -> NorthwardResult<Box<dyn Plugin>> {
        let app_id = ctx.app_id;
        let inner_plugin = self.inner.create_plugin(ctx)?;
        let (tx, rx) = mpsc::channel(self.channel_capacity);
        Ok(Box::new(RuntimeAwarePlugin {
            inner: Arc::new(inner_plugin),
            app_id,
            tx,
            cancel_token: CancellationToken::new(),
            rx: std::sync::Mutex::new(Some(rx)),
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
    rx: std::sync::Mutex<Option<mpsc::Receiver<Arc<NorthwardData>>>>,
    rt_handle: Option<Handle>,
}

#[async_trait::async_trait]
impl Plugin for RuntimeAwarePlugin {
    async fn start(&self) -> NorthwardResult<()> {
        let handle = self
            .rt_handle
            .clone()
            .ok_or_else(|| NorthwardError::RuntimeError {
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

        rx_res.await.unwrap_or(Err(NorthwardError::RuntimeError {
            reason: "Plugin start task cancelled".to_string(),
        }))
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

    async fn stop(&self) -> NorthwardResult<()> {
        self.cancel_token.cancel();
        Ok(())
    }
}
