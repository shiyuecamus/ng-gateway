//! AppActor: Independent runtime for each northward app
//!
//! Each AppActor manages a single northward app with:
//! - Independent data queue (Gateway -> Plugin)
//! - Independent events channel (Plugin -> Gateway)
//! - Own worker task with CancellationToken control
//! - Lock-free metrics and config hot-reload

use super::observer::ConnectionStateCell;
use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use ng_gateway_common::metrics::{
    channel::{bounded, InstrumentedReceiver, InstrumentedSender, QueueObserver},
    northward::NorthwardAppMetricHandles,
    queue::DropReason,
    NGMetricsHub,
};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::core::metrics::{AppActorState, NorthwardAppMetricsSnapshot};
use ng_gateway_sdk::{
    ConnectionState, DropPolicy, NorthwardData, NorthwardEvent, Phase, Plugin, PluginConfig,
    QueuePolicy,
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::mpsc::{
        self,
        error::{SendTimeoutError, TrySendError},
    },
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn, Instrument, Span};

/// Type alias for buffer queue storing (data, timestamp) pairs
///
/// Used to buffer data when plugin is not connected.
/// Data is flushed when connection is established.
pub(crate) type BufferQueue = Arc<Mutex<VecDeque<(Arc<NorthwardData>, Instant)>>>;

/// Pre-built per-app I/O and metric handles.
///
/// This is created by the host (northward manager) so it can be shared with:
/// - the AppActor (data-plane send/buffer paths)
/// - the supervision Observer (control-plane flush/metrics paths)
pub struct AppIo {
    pub prom: Arc<NorthwardAppMetricHandles>,
    pub data_tx: InstrumentedSender<Arc<NorthwardData>>,
    pub data_rx: InstrumentedReceiver<Arc<NorthwardData>>,
    pub buffer_queue: BufferQueue,
    pub buffer_observer: QueueObserver,
    /// Host-owned app connection state snapshot (updated by observer).
    pub conn_state: ConnectionStateCell,
}

impl AppIo {
    /// Create all per-app metric handles and bounded queues.
    ///
    /// # Notes
    /// This is NOT a hot-path function; it runs during app bootstrap.
    pub(crate) fn new(
        metrics_hub: &NGMetricsHub,
        app_id: i32,
        plugin_id: i32,
        queue_policy: QueuePolicy,
    ) -> NGResult<Self> {
        let prom = metrics_hub.register_northward_app_metrics(app_id, plugin_id)?;
        let (data_tx, data_rx) = bounded(
            metrics_hub,
            format!("northward_app_{app_id}"),
            queue_policy.capacity as usize,
        )?;
        let buffer_observer = QueueObserver::new(
            metrics_hub,
            format!("northward_app_buffer_{app_id}"),
            queue_policy.buffer_capacity as u64,
        )?;
        Ok(Self {
            prom,
            data_tx,
            data_rx,
            buffer_queue: Arc::new(Mutex::new(VecDeque::new())),
            buffer_observer,
            conn_state: Arc::new(ArcSwap::from_pointee(ConnectionState::now(
                Phase::Disconnected,
                0,
            ))),
        })
    }
}

/// Independent actor for each northward app
///
/// Design principles:
/// - Per-app isolation: Each app has independent channels, state, metrics
/// - Lock-free hot path: Config uses ArcSwap, metrics use Atomic
/// - CancellationToken-only control: No JoinHandle stored, clean shutdown
/// - Gateway-managed events: events_rx is taken by Gateway for bridge task
pub struct AppActor {
    // === Basic identity ===
    app_id: i32,
    app_name: String,
    plugin_id: i32,
    /// Per-app tracing span to make per-app log overrides reliable.
    span: Span,

    // === Plugin instance ===
    plugin: Arc<dyn Plugin>,

    // === Connection state snapshot (host-owned, observer-updated) ===
    conn_state: ConnectionStateCell,

    // === Configuration (hot-reloadable, lock-free reads) ===
    config: Arc<dyn PluginConfig>,

    // === Policy configuration (immutable after creation) ===
    queue_policy: QueuePolicy,

    // === Data queue (Gateway -> Plugin) ===
    data_tx: InstrumentedSender<Arc<NorthwardData>>,
    /// Receiver is taken once on start() and moved into worker task
    data_rx: Mutex<Option<InstrumentedReceiver<Arc<NorthwardData>>>>,

    // === Events channel (Plugin -> Gateway) ===
    /// Gateway takes this via take_events_rx() to start bridge task
    events_rx: Mutex<Option<mpsc::Receiver<NorthwardEvent>>>,

    // === Control (CancellationToken-only design) ===
    shutdown_token: CancellationToken,

    // === State (atomic) ===
    state: AtomicU8,

    // === Metrics ===
    /// Prometheus metric handles (pre-resolved) owned by `NGMetricsHub`.
    prom: Arc<NorthwardAppMetricHandles>,

    // === Buffer queue for unconnected state ===
    /// Buffer queue: (data, timestamp) pairs
    /// Data is buffered here when plugin is not connected and flushed when connected
    buffer_queue: BufferQueue,
    /// Buffer queue observer for Prometheus queue metrics.
    buffer_observer: QueueObserver,

    // === Timestamps ===
    created_at: DateTime<Utc>,
}

/// Parameters required to construct an `AppActor`.
///
/// # Rationale
/// This bundles construction inputs to keep `AppActor::new` ergonomic and
/// to satisfy `clippy::too_many_arguments` under `-D warnings`.
pub struct AppActorParams {
    /// Application ID.
    pub app_id: i32,
    /// Application name.
    pub app_name: String,
    /// Plugin ID.
    pub plugin_id: i32,
    /// Plugin instance (already initialized, will be wrapped in `Arc`).
    pub plugin: Box<dyn Plugin>,
    /// Events receiver from the plugin.
    pub events_rx: mpsc::Receiver<NorthwardEvent>,
    /// Plugin configuration.
    pub config: Arc<dyn PluginConfig>,
    /// Queue policy for backpressure handling.
    pub queue_policy: QueuePolicy,
    /// Cancellation token (usually a child token from Gateway).
    pub shutdown_token: CancellationToken,
    /// Pre-built per-app I/O bundle.
    pub io: AppIo,
}

impl AppActor {
    /// Create a new AppActor
    ///
    /// # Arguments
    /// * `app_id` - Application ID
    /// * `app_name` - Application name
    /// * `plugin_id` - Plugin ID
    /// * `plugin` - Plugin instance (already initialized, will be wrapped in Arc)
    /// * `events_rx` - Events receiver from the plugin
    /// * `config` - Plugin configuration
    /// * `queue_policy` - Queue policy for handling backpressure
    /// * `retry_policy` - Retry policy for failed sends
    /// * `shutdown_token` - Cancellation token (usually a child token from Gateway)
    ///
    /// # Returns
    /// Returns NGResult<Self> - currently always succeeds but returns Result for future extensibility
    ///
    /// # Notes
    /// The plugin must be initialized (init() called) before passing to this constructor,
    /// as this function wraps it in Arc, making mutable access impossible.
    pub fn new(params: AppActorParams) -> Self {
        Self {
            app_id: params.app_id,
            app_name: params.app_name,
            plugin_id: params.plugin_id,
            span: tracing::info_span!(
                "northward-app",
                app_id = params.app_id,
                plugin_id = params.plugin_id
            ),
            plugin: Arc::from(params.plugin),
            conn_state: params.io.conn_state,
            config: params.config,
            queue_policy: params.queue_policy,
            data_tx: params.io.data_tx,
            data_rx: Mutex::new(Some(params.io.data_rx)),
            events_rx: Mutex::new(Some(params.events_rx)),
            shutdown_token: params.shutdown_token,
            state: AtomicU8::new(AppActorState::Uninitialized as u8),
            prom: params.io.prom,
            buffer_queue: params.io.buffer_queue,
            buffer_observer: params.io.buffer_observer,
            created_at: Utc::now(),
        }
    }

    /// Get a consistent northward metrics snapshot for REST/WS.
    #[inline]
    pub fn metrics_snapshot(&self) -> NorthwardAppMetricsSnapshot {
        self.prom.snapshot()
    }

    // === Accessors ===

    #[inline]
    pub fn app_id(&self) -> i32 {
        self.app_id
    }

    #[inline]
    pub fn app_name(&self) -> String {
        self.app_name.clone()
    }

    #[inline]
    pub fn plugin_id(&self) -> i32 {
        self.plugin_id
    }

    #[inline]
    pub fn state(&self) -> AppActorState {
        AppActorState::from(self.state.load(Ordering::Acquire))
    }

    #[inline]
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Get current configuration (lock-free)
    #[inline]
    pub fn get_config(&self) -> Arc<dyn PluginConfig> {
        Arc::clone(&self.config)
    }

    /// Hot-reload configuration (lock-free write)
    ///
    /// The new config will be picked up by the worker task on next iteration
    pub fn update_config(&mut self, new_config: Arc<dyn PluginConfig>) {
        self.config = Arc::clone(&new_config);
    }

    // === Events channel management ===

    /// Gateway calls this to take ownership of events_rx for bridge task
    ///
    /// Can only be called once. Returns None if already taken.
    pub fn take_events_rx(&self) -> Option<mpsc::Receiver<NorthwardEvent>> {
        self.events_rx.lock().unwrap().take()
    }

    // === Data sending with queue_policy-based backpressure ===

    /// Send data to this app with backpressure handling and connection state check
    ///
    /// Applies the configured queue_policy to handle queue overflow:
    /// - `DropPolicy::Discard`: Drop current item if queue is full (non-blocking)
    /// - `DropPolicy::Block`: Block until space is available or timeout expires
    ///
    /// If buffer is enabled and plugin is not connected, data will be buffered
    /// and flushed when connection is established.
    ///
    /// Note: Change detection filtering is now handled at the routing level in NGNorthwardManager.
    ///
    /// # Returns
    /// - `Ok(true)` if data was sent successfully or buffered
    /// - `Ok(false)` if data was dropped (Discard policy)
    /// - `Err` if app is not running or channel closed
    pub async fn send_data(&self, data: Arc<NorthwardData>) -> NGResult<bool> {
        let _enter = self.span.enter();
        let state = self.state();
        if state != AppActorState::Running {
            return Err(NGError::InvalidStateError(format!(
                "App {} is not running (state: {:?})",
                self.app_id, state
            )));
        }

        // Check connection state (snapshot stream)
        let conn_state = self.plugin.subscribe_connection_state().borrow().clone();
        let is_connected = conn_state.is_connected();

        // If not connected and buffer is enabled, buffer the data
        if !is_connected && self.queue_policy.buffer_enabled {
            return self.buffer_data(data).await;
        }

        // If not connected and buffer is disabled, drop the data
        if !is_connected {
            debug!(
                app_id = self.app_id,
                "Data dropped: plugin not connected and buffer disabled"
            );
            self.prom.record_uplink_dropped();
            return Ok(false);
        }

        match self.queue_policy.drop_policy {
            DropPolicy::Discard => {
                // Non-blocking: drop current item if queue is full
                match self.data_tx.try_send(data) {
                    Ok(_) => Ok(true),
                    Err(TrySendError::Full(_)) => {
                        // Queue full, discard current item
                        debug!(
                            app_id = self.app_id,
                            capacity = self.queue_policy.capacity,
                            "Data dropped: queue full (policy: Discard)"
                        );
                        self.prom.record_uplink_dropped();
                        Ok(false)
                    }
                    Err(TrySendError::Closed(_)) => {
                        error!("App {} data channel closed", self.app_id);
                        Err(NGError::Error(format!(
                            "App {} data channel closed",
                            self.app_id
                        )))
                    }
                }
            }
            DropPolicy::Block => {
                // Blocking with timeout: wait for space or drop after timeout
                let timeout_duration = Duration::from_millis(self.queue_policy.block_duration);

                match self.data_tx.send_timeout(data, timeout_duration).await {
                    Ok(()) => Ok(true),
                    Err(SendTimeoutError::Timeout(_)) => {
                        warn!(
                            app_id = self.app_id,
                            timeout_ms = self.queue_policy.block_duration,
                            capacity = self.queue_policy.capacity,
                            "Data dropped: send timeout (policy: Block)"
                        );
                        self.prom.record_uplink_dropped();
                        Ok(false)
                    }
                    Err(SendTimeoutError::Closed(_)) => {
                        error!("App {} data channel closed", self.app_id);
                        Err(NGError::Error(format!(
                            "App {} data channel closed",
                            self.app_id
                        )))
                    }
                }
            }
        }
    }

    /// Buffer data when plugin is not connected
    ///
    /// # Returns
    /// - `Ok(true)` if data was buffered successfully
    /// - `Ok(false)` if buffer is full and oldest item was dropped (FIFO)
    async fn buffer_data(&self, data: Arc<NorthwardData>) -> NGResult<bool> {
        let _enter = self.span.enter();
        let mut buffer = self.buffer_queue.lock().unwrap();
        let now = Instant::now();

        // If buffer is enabled but capacity is zero, drop immediately to avoid unbounded growth.
        if self.queue_policy.buffer_capacity == 0 {
            self.buffer_observer.dropped(DropReason::BufferFull);
            self.prom.record_uplink_dropped();
            return Ok(false);
        }

        // Check buffer capacity
        if buffer.len() >= self.queue_policy.buffer_capacity as usize {
            // Buffer full: remove oldest item (FIFO)
            if buffer.pop_front().is_some() {
                self.buffer_observer.dec();
            }
            debug!(
                app_id = self.app_id,
                buffer_capacity = self.queue_policy.buffer_capacity,
                "Buffer full: dropped oldest item (FIFO)"
            );
            self.buffer_observer.dropped(DropReason::BufferFull);
            self.prom.record_uplink_dropped();
        }

        // Add new data to buffer
        buffer.push_back((data, now));
        self.buffer_observer.inc();
        debug!(
            app_id = self.app_id,
            buffer_size = buffer.len(),
            "Data buffered (plugin not connected)"
        );

        Ok(true)
    }

    /// Internal helper function to flush buffered data to queue
    ///
    /// Removes expired items and sends remaining data to queue.
    /// This is a static function that can be called from both instance methods
    /// and async tasks that have moved ownership of the required fields.
    ///
    /// # Arguments
    /// * `buffer_queue` - Shared buffer queue
    /// * `data_tx` - Data channel sender
    /// * `queue_policy` - Queue policy configuration
    /// * `app_id` - Application ID for logging
    /// * `metrics` - Metrics instance for tracking
    ///
    /// # Returns
    /// * `Ok(flushed_count)` if flush completed successfully
    /// * `Err` if data channel was closed during flush
    pub(crate) fn flush_buffer(
        buffer_queue: &BufferQueue,
        data_tx: &InstrumentedSender<Arc<NorthwardData>>,
        queue_policy: QueuePolicy,
        app_id: i32,
        prom: &Arc<NorthwardAppMetricHandles>,
        buffer_observer: &QueueObserver,
    ) -> Result<usize, NGError> {
        let mut buffer = buffer_queue.lock().unwrap();
        let now = Instant::now();
        let expire_duration = if queue_policy.buffer_expire_ms > 0 {
            Some(Duration::from_millis(queue_policy.buffer_expire_ms))
        } else {
            None
        };

        let mut flushed = 0;
        let mut to_send = Vec::new();
        let mut channel_closed = false;
        let mut current_item: Option<(Arc<NorthwardData>, Instant)> = None;

        // Collect non-expired items
        while let Some((data, timestamp)) = buffer.pop_front() {
            buffer_observer.dec();
            // Check expiration
            if let Some(expire) = expire_duration {
                if now.duration_since(timestamp) > expire {
                    // Item expired, skip it
                    debug!(app_id = app_id, "Buffered data expired, skipping");
                    buffer_observer.dropped(DropReason::Expired);
                    prom.record_uplink_dropped();
                    continue;
                }
            }

            // Try to send to queue
            match data_tx.try_send(data) {
                Ok(_) => {
                    flushed += 1;
                }
                Err(TrySendError::Full(data)) => {
                    // Queue full, keep in buffer for next flush
                    to_send.push((data, timestamp));
                }
                Err(TrySendError::Closed(data)) => {
                    // Channel closed, stop flushing
                    error!("App {} data channel closed during buffer flush", app_id);
                    // Save current item to put back
                    current_item = Some((data, timestamp));
                    channel_closed = true;
                    break;
                }
            }
        }

        if channel_closed {
            // Put current item back
            if let Some(item) = current_item {
                buffer.push_front(item);
                buffer_observer.inc();
            }
            // Put remaining items back to buffer
            for item in to_send.into_iter().rev() {
                buffer.push_front(item);
                buffer_observer.inc();
            }
            Err(NGError::Error(format!(
                "App {} data channel closed",
                app_id
            )))
        } else {
            // Put back items that couldn't be sent (queue full)
            for item in to_send.into_iter().rev() {
                buffer.push_front(item);
                buffer_observer.inc();
            }

            if flushed > 0 {
                info!(
                    app_id = app_id,
                    flushed_count = flushed,
                    "Flushed {} buffered items to queue",
                    flushed
                );
            }

            Ok(flushed)
        }
    }

    /// Get an instrumented data sender for this app.
    ///
    /// # Notes
    /// Prefer calling `send_data()` from routing paths because it applies connection checks
    /// and queue policies. This accessor is provided for internal helpers that need direct
    /// send/try_send semantics while preserving queue metrics.
    #[inline]
    pub fn data_tx(&self) -> InstrumentedSender<Arc<NorthwardData>> {
        self.data_tx.clone()
    }

    /// Wait until plugin connection reaches Connected or Failed, with timeout.
    ///
    /// This mirrors the southbound `wait_for_final` behavior and is used by
    /// lifecycle helpers to provide a unified "start + wait" semantic.
    pub async fn wait_for_connected(&self, timeout_ms: u64) -> NGResult<()> {
        let _enter = self.span.enter();
        let mut rx = self.plugin.subscribe_connection_state();

        match timeout(Duration::from_millis(timeout_ms), async move {
            rx.wait_for(|state| matches!(state.phase, Phase::Connected | Phase::Failed))
                .await
                .map(|r| r.clone())
        })
        .await
        {
            Ok(Ok(state)) => {
                if state.phase == Phase::Connected {
                    return Ok(());
                }
                if state.phase == Phase::Failed {
                    let reason = state
                        .last_failure
                        .as_ref()
                        .map(|r| r.summary.as_ref())
                        .unwrap_or("unknown failure");
                    return Err(NGError::Error(format!(
                        "Plugin connection failed: {reason}"
                    )));
                }
                Err(NGError::Error("Invalid connection phase".to_string()))
            }
            Ok(Err(_)) => Err(NGError::Error(
                "Plugin connection state channel closed".to_string(),
            )),
            Err(_) => Err(NGError::Error(format!(
                "Plugin connection timeout after {} ms",
                timeout_ms
            ))),
        }
    }
    // === Worker task management ===

    /// Spawn worker task that consumes data from queue and sends to plugin
    fn spawn_worker_task(&self) {
        let app_id = self.app_id;
        let plugin = Arc::clone(&self.plugin);
        let prom = Arc::clone(&self.prom);
        let token = self.shutdown_token.clone();
        let span = self.span.clone();

        // Take the receiver (can only be done once)
        let mut rx = match self.data_rx.lock().unwrap().take() {
            Some(rx) => rx,
            None => {
                error!("App {} worker already spawned or rx already taken", app_id);
                return;
            }
        };

        tokio::spawn(async move {
            info!("App {} worker task started", app_id);

            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        info!("App {} worker task cancelled", app_id);
                        break;
                    }
                    maybe_data = rx.recv() => {
                        match maybe_data {
                            Some(data) => {
                                // Check connection state before processing
                                let conn_state = plugin.subscribe_connection_state().borrow().clone();
                                let is_connected = conn_state.is_connected();

                                if !is_connected {
                                    // Not connected: skip processing
                                    debug!(
                                        app_id = app_id,
                                        "Data skipped: plugin not connected"
                                    );
                                    prom.record_uplink_dropped();
                                    continue;
                                }

                                let start = Instant::now();

                                // Process data through plugin
                                match plugin.process_data(Arc::clone(&data)).await {
                                    Ok(_) => {
                                        let elapsed = start.elapsed();
                                        prom.record_uplink_success(
                                            elapsed.as_nanos() as u64,
                                            elapsed.as_secs_f64(),
                                        );

                                        debug!("App {} processed data successfully", app_id);
                                    }
                                    Err(e) => {
                                        error!("App {} failed to process data: {}", app_id, e);
                                        prom.record_uplink_fail(start.elapsed().as_secs_f64());
                                    }
                                }
                            }
                            None => {
                                warn!("App {} data channel closed", app_id);
                                break;
                            }
                        }
                    }
                }
            }
            info!("App {} worker task stopped", app_id);
        }
        .instrument(span));
    }

    // === Lifecycle management ===

    /// Start the app actor (aligned with southbound ChannelMonitor pattern)
    ///
    /// This method:
    /// 1. Spawns the data worker task to process data queue
    /// 2. Subscribes to plugin connection state (plugin manages its own connection)
    /// 3. Spawns connection monitor task (observer pattern)
    /// 4. Transitions to Running state
    ///
    /// # Prerequisites
    /// The plugin must already be initialized (plugin.init() must have been called
    /// before AppActor was created). The plugin's internal connection supervisor
    /// task is already running and managing connections.
    ///
    /// # Design Philosophy
    /// - Plugin is self-supervised (manages its own connection lifecycle)
    /// - AppActor subscribes to connection state changes (observer pattern)
    /// - Fully aligned with southbound Driver + ChannelMonitor design
    pub async fn start(&self) -> NGResult<()> {
        let _enter = self.span.enter();
        let current_state = self.state();
        if current_state != AppActorState::Uninitialized && current_state != AppActorState::Stopped
        {
            return Err(NGError::InvalidStateError(format!(
                "Cannot start app {} from state {:?}",
                self.app_id, current_state
            )));
        }

        self.state
            .store(AppActorState::Starting as u8, Ordering::Release);
        self.prom.set_state(AppActorState::Starting as u8 as i64);
        info!(
            "Starting app {} (plugin_id: {})",
            self.app_id, self.plugin_id
        );

        // 0. Start plugin connection supervisor asynchronously
        {
            let plugin = Arc::clone(&self.plugin);
            let app_id = self.app_id;
            let span = self.span.clone();
            tokio::spawn(
                async move {
                    if let Err(e) = plugin.start().await {
                        error!("App {} plugin start failed: {}", app_id, e);
                    }
                }
                .instrument(span),
            );
        }

        // 1. Spawn data worker task
        self.spawn_worker_task();

        // 2. Transition to Running (connection state side effects are handled by supervision Observer)
        self.state
            .store(AppActorState::Running as u8, Ordering::Release);
        self.prom.set_state(AppActorState::Running as u8 as i64);
        info!(
            "App {} started (connection managed by plugin supervisor)",
            self.app_id
        );

        Ok(())
    }

    /// Stop the app actor gracefully
    ///
    /// This will:
    /// 1. Cancel the shutdown token (signals all tasks to stop)
    /// 2. Stop the plugin (cancels internal supervisor, disconnects)
    /// 3. Transition to Stopped state
    pub async fn stop(&self) {
        let _enter = self.span.enter();
        let current_state = self.state();
        if current_state == AppActorState::Stopped || current_state == AppActorState::Uninitialized
        {
            warn!(
                "App {} already stopped (state: {:?})",
                self.app_id, current_state
            );
            return;
        }

        self.state
            .store(AppActorState::Stopping as u8, Ordering::Release);
        self.prom.set_state(AppActorState::Stopping as u8 as i64);
        info!("Stopping app {}", self.app_id);

        // 1. Cancel the shutdown token (stops worker and monitor tasks)
        self.shutdown_token.cancel();

        // 2. Stop plugin (cancels supervisor, disconnects)
        let timeout_duration = Duration::from_secs(5);
        if let Err(e) = timeout(timeout_duration, self.plugin.stop()).await {
            error!("App {} plugin stop timeout: {e}", self.app_id);
        }

        // 3. Transition to Stopped
        self.state
            .store(AppActorState::Stopped as u8, Ordering::Release);
        self.prom.set_state(AppActorState::Stopped as u8 as i64);
        info!("App {} stopped", self.app_id);
    }

    /// Get current connection state from plugin
    pub fn get_connection_state(&self) -> Arc<ConnectionState> {
        self.conn_state.load_full()
    }
}
