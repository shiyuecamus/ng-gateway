//! AppActor: Independent runtime for each northward app
//!
//! Each AppActor manages a single northward app with:
//! - Independent data queue (Gateway -> Plugin)
//! - Independent events channel (Plugin -> Gateway)
//! - Own worker task with CancellationToken control
//! - Lock-free metrics and config hot-reload

use chrono::{DateTime, Utc};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::core::metrics::{AppState, NorthwardAppMetrics};
use ng_gateway_sdk::{
    DropPolicy, NorthwardConnectionState, NorthwardData, NorthwardEvent, Plugin, PluginConfig,
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
    sync::{
        mpsc::{self, error::TrySendError},
        watch,
    },
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Type alias for buffer queue storing (data, timestamp) pairs
///
/// Used to buffer data when plugin is not connected.
/// Data is flushed when connection is established.
type BufferQueue = Arc<Mutex<VecDeque<(Arc<NorthwardData>, Instant)>>>;

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

    // === Plugin instance ===
    plugin: Arc<dyn Plugin>,

    // === Configuration (hot-reloadable, lock-free reads) ===
    config: Arc<dyn PluginConfig>,

    // === Policy configuration (immutable after creation) ===
    queue_policy: QueuePolicy,

    // === Data queue (Gateway -> Plugin) ===
    data_tx: mpsc::Sender<Arc<NorthwardData>>,
    /// Receiver is taken once on start() and moved into worker task
    data_rx: Mutex<Option<mpsc::Receiver<Arc<NorthwardData>>>>,

    // === Events channel (Plugin -> Gateway) ===
    /// Gateway takes this via take_events_rx() to start bridge task
    events_rx: Mutex<Option<mpsc::Receiver<NorthwardEvent>>>,

    // === Control (CancellationToken-only design) ===
    shutdown_token: CancellationToken,

    // === State (atomic) ===
    state: AtomicU8,

    // === Metrics (lock-free counters) ===
    pub metrics: Arc<NorthwardAppMetrics>,

    // === Buffer queue for unconnected state ===
    /// Buffer queue: (data, timestamp) pairs
    /// Data is buffered here when plugin is not connected and flushed when connected
    buffer_queue: BufferQueue,

    // === Timestamps ===
    created_at: DateTime<Utc>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        app_id: i32,
        app_name: String,
        plugin_id: i32,
        plugin: Box<dyn Plugin>,
        events_rx: mpsc::Receiver<NorthwardEvent>,
        config: Arc<dyn PluginConfig>,
        queue_policy: QueuePolicy,
        shutdown_token: CancellationToken,
    ) -> NGResult<Self> {
        // Create bounded data channel
        let (data_tx, data_rx) = mpsc::channel(queue_policy.capacity as usize);

        // Wrap in Arc after initialization (zero runtime overhead for hot path)
        let plugin = Arc::from(plugin);

        Ok(Self {
            app_id,
            app_name,
            plugin_id,
            plugin,
            config,
            queue_policy,
            data_tx,
            data_rx: Mutex::new(Some(data_rx)),
            events_rx: Mutex::new(Some(events_rx)),
            shutdown_token,
            state: AtomicU8::new(AppState::Uninitialized as u8),
            metrics: Arc::new(NorthwardAppMetrics::default()),
            buffer_queue: Arc::new(Mutex::new(VecDeque::new())),
            created_at: Utc::now(),
        })
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
    pub fn state(&self) -> AppState {
        AppState::from(self.state.load(Ordering::Acquire))
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
        let state = self.state();
        if state != AppState::Running {
            return Err(NGError::InvalidStateError(format!(
                "App {} is not running (state: {:?})",
                self.app_id, state
            )));
        }

        // Check connection state
        let conn_state = self.plugin.subscribe_connection_state().borrow().clone();
        let is_connected = matches!(conn_state, NorthwardConnectionState::Connected);

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
            self.metrics.increment_dropped();
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
                        self.metrics.increment_dropped();
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

                match timeout(timeout_duration, self.data_tx.send(data)).await {
                    Ok(Ok(_)) => Ok(true),
                    Ok(Err(_)) => {
                        // Channel closed
                        error!("App {} data channel closed", self.app_id);
                        Err(NGError::Error(format!(
                            "App {} data channel closed",
                            self.app_id
                        )))
                    }
                    Err(_) => {
                        // Timeout expired, drop current item
                        warn!(
                            app_id = self.app_id,
                            timeout_ms = self.queue_policy.block_duration,
                            capacity = self.queue_policy.capacity,
                            "Data dropped: send timeout (policy: Block)"
                        );
                        self.metrics.increment_dropped();
                        Ok(false)
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
        let mut buffer = self.buffer_queue.lock().unwrap();
        let now = Instant::now();

        // Check buffer capacity
        if buffer.len() >= self.queue_policy.buffer_capacity as usize {
            // Buffer full: remove oldest item (FIFO)
            buffer.pop_front();
            debug!(
                app_id = self.app_id,
                buffer_capacity = self.queue_policy.buffer_capacity,
                "Buffer full: dropped oldest item (FIFO)"
            );
            self.metrics.increment_dropped();
        }

        // Add new data to buffer
        buffer.push_back((data, now));
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
    fn flush_buffer(
        buffer_queue: &BufferQueue,
        data_tx: &mpsc::Sender<Arc<NorthwardData>>,
        queue_policy: QueuePolicy,
        app_id: i32,
        metrics: &Arc<NorthwardAppMetrics>,
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
            // Check expiration
            if let Some(expire) = expire_duration {
                if now.duration_since(timestamp) > expire {
                    // Item expired, skip it
                    debug!(app_id = app_id, "Buffered data expired, skipping");
                    metrics.increment_dropped();
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
            }
            // Put remaining items back to buffer
            for item in to_send.into_iter().rev() {
                buffer.push_front(item);
            }
            Err(NGError::Error(format!(
                "App {} data channel closed",
                app_id
            )))
        } else {
            // Put back items that couldn't be sent (queue full)
            for item in to_send.into_iter().rev() {
                buffer.push_front(item);
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

    /// Get data sender for blocking sends (used internally)
    pub fn data_tx(&self) -> mpsc::Sender<Arc<NorthwardData>> {
        self.data_tx.clone()
    }

    /// Wait until plugin connection reaches Connected or Failed, with timeout.
    ///
    /// This mirrors the southbound `wait_for_final` behavior and is used by
    /// lifecycle helpers to provide a unified "start + wait" semantic.
    pub async fn wait_for_connected(&self, timeout_ms: u64) -> NGResult<()> {
        let mut rx = self.plugin.subscribe_connection_state();

        match timeout(Duration::from_millis(timeout_ms), async move {
            rx.wait_for(|state| {
                matches!(
                    state,
                    NorthwardConnectionState::Connected | NorthwardConnectionState::Failed(_)
                )
            })
            .await
            .map(|r| r.clone())
        })
        .await
        {
            Ok(Ok(state)) => match state {
                NorthwardConnectionState::Connected => Ok(()),
                NorthwardConnectionState::Failed(reason) => Err(NGError::Error(format!(
                    "Plugin connection failed: {}",
                    reason
                ))),
                _ => Err(NGError::Error("Invalid connection state".to_string())),
            },
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
        let metrics = Arc::clone(&self.metrics);
        let token = self.shutdown_token.clone();

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
                                let is_connected = matches!(conn_state, NorthwardConnectionState::Connected);

                                if !is_connected {
                                    // Not connected: skip processing
                                    debug!(
                                        app_id = app_id,
                                        "Data skipped: plugin not connected"
                                    );
                                    metrics.increment_dropped();
                                    continue;
                                }

                                let start = Instant::now();

                                // Process data through plugin
                                match plugin.process_data(Arc::clone(&data)).await {
                                    Ok(_) => {
                                        metrics.increment_sent();

                                        // Update latency metrics
                                        let elapsed_ns = start.elapsed().as_nanos() as u64;
                                        metrics.update_latency(elapsed_ns);

                                        debug!("App {} processed data successfully", app_id);
                                    }
                                    Err(e) => {
                                        error!("App {} failed to process data: {}", app_id, e);
                                        metrics.increment_errors();
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
        });
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
        let current_state = self.state();
        if current_state != AppState::Uninitialized && current_state != AppState::Stopped {
            return Err(NGError::InvalidStateError(format!(
                "Cannot start app {} from state {:?}",
                self.app_id, current_state
            )));
        }

        self.state
            .store(AppState::Starting as u8, Ordering::Release);
        info!(
            "Starting app {} (plugin_id: {})",
            self.app_id, self.plugin_id
        );

        // 0. Start plugin connection supervisor asynchronously
        {
            let plugin = Arc::clone(&self.plugin);
            let app_id = self.app_id;
            tokio::spawn(async move {
                if let Err(e) = plugin.start().await {
                    error!("App {} plugin start failed: {}", app_id, e);
                }
            });
        }

        // 1. Spawn data worker task
        self.spawn_worker_task();

        // 2. Subscribe to plugin connection state and spawn monitor
        let state_rx = self.plugin.subscribe_connection_state();
        self.spawn_connection_monitor(state_rx);

        // 3. Transition to Running (actual connection state tracked via monitor)
        self.state.store(AppState::Running as u8, Ordering::Release);
        info!(
            "App {} started (connection managed by plugin supervisor)",
            self.app_id
        );

        Ok(())
    }

    /// Spawn connection state monitor task (aligned with ChannelMonitor)
    ///
    /// This task subscribes to the plugin's connection state and:
    /// - Logs connection state transitions
    /// - Updates metrics
    /// - Flushes buffered data when connection is established
    ///
    /// Note: AppActor's state is managed separately and can be queried via get_connection_state()
    ///
    /// The task automatically terminates when:
    /// - shutdown_token is cancelled
    /// - Plugin's state channel is closed
    fn spawn_connection_monitor(&self, mut state_rx: watch::Receiver<NorthwardConnectionState>) {
        let app_id = self.app_id;
        let app_name = self.app_name.clone();
        let metrics = Arc::clone(&self.metrics);
        let token = self.shutdown_token.clone();
        let buffer_queue = Arc::clone(&self.buffer_queue);
        let queue_policy = self.queue_policy;
        let data_tx = self.data_tx.clone();

        tokio::spawn(async move {
            info!(
                "Connection monitor started for app {} ({})",
                app_id, app_name
            );

            // Track last state to avoid duplicate processing
            let mut last_state = state_rx.borrow().clone();

            // Log initial state
            Self::log_connection_state(app_id, &app_name, &last_state);

            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        info!("Connection monitor cancelled for app {}", app_id);
                        break;
                    }
                    result = state_rx.changed() => {
                        if result.is_err() {
                            warn!("Connection state channel closed for app {}", app_id);
                            break;
                        }

                        let conn_state = state_rx.borrow().clone();
                        if conn_state == last_state {
                            continue;
                        }

                        Self::log_connection_state(app_id, &app_name, &conn_state);

                        // Handle state transitions: update metrics and flush buffer
                        match conn_state {
                            NorthwardConnectionState::Failed(_) => {
                                metrics.increment_errors();
                            }
                            NorthwardConnectionState::Connected if queue_policy.buffer_enabled => {
                                // Flush buffered data when connection is established
                                if let Err(e) = Self::flush_buffer(
                                    &buffer_queue,
                                    &data_tx,
                                    queue_policy,
                                    app_id,
                                    &metrics,
                                ) {
                                    warn!(
                                        app_id = app_id,
                                        error = %e,
                                        "Failed to flush buffer after connection"
                                    );
                                }
                            }
                            _ => {
                                // Other states: no action needed
                            }
                        }

                        last_state = conn_state;
                    }
                }
            }

            info!("Connection monitor stopped for app {}", app_id);
        });
    }

    /// Log connection state transitions
    fn log_connection_state(app_id: i32, app_name: &str, conn_state: &NorthwardConnectionState) {
        match conn_state {
            NorthwardConnectionState::Connected => {
                info!("App {} ({}) connected", app_id, app_name);
            }
            NorthwardConnectionState::Disconnected => {
                warn!("App {} ({}) disconnected", app_id, app_name);
            }
            NorthwardConnectionState::Connecting => {
                debug!("App {} ({}) connecting", app_id, app_name);
            }
            NorthwardConnectionState::Reconnecting => {
                debug!("App {} ({}) reconnecting", app_id, app_name);
            }
            NorthwardConnectionState::Failed(reason) => {
                error!(
                    "App {} ({}) connection failed: {}",
                    app_id, app_name, reason
                );
            }
        }
    }

    /// Stop the app actor gracefully
    ///
    /// This will:
    /// 1. Cancel the shutdown token (signals all tasks to stop)
    /// 2. Stop the plugin (cancels internal supervisor, disconnects)
    /// 3. Transition to Stopped state
    pub async fn stop(&self) {
        let current_state = self.state();
        if current_state == AppState::Stopped || current_state == AppState::Uninitialized {
            warn!(
                "App {} already stopped (state: {:?})",
                self.app_id, current_state
            );
            return;
        }

        self.state
            .store(AppState::Stopping as u8, Ordering::Release);
        info!("Stopping app {}", self.app_id);

        // 1. Cancel the shutdown token (stops worker and monitor tasks)
        self.shutdown_token.cancel();

        // 2. Stop plugin (cancels supervisor, disconnects)
        let timeout_duration = Duration::from_secs(5);
        if let Err(e) = timeout(timeout_duration, self.plugin.stop()).await {
            error!("App {} plugin stop timeout: {e}", self.app_id);
        }

        // 3. Transition to Stopped
        self.state.store(AppState::Stopped as u8, Ordering::Release);
        info!("App {} stopped", self.app_id);
    }

    /// Get current connection state from plugin
    pub fn get_connection_state(&self) -> NorthwardConnectionState {
        self.plugin.subscribe_connection_state().borrow().clone()
    }

    /// Ping the plugin to check latency
    pub async fn ping(&self) -> NGResult<std::time::Duration> {
        self.plugin
            .ping()
            .await
            .map_err(|e| NGError::Error(format!("Ping failed: {}", e)))
    }
}
