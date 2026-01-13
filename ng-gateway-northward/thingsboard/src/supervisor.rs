use super::{
    config::ThingsBoardPluginConfig, mqtt::connect_mqtt_client, provision::ProvisionCredentials,
    topics::Topics,
};
use arc_swap::ArcSwapOption;
use backoff::backoff::Backoff;
use ng_gateway_sdk::{
    build_exponential_backoff, NorthwardConnectionState, NorthwardError, NorthwardResult,
    RetryPolicy,
};
use rumqttc::{AsyncClient, Event, EventLoop, Packet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Shared client entry for lock-free access pattern (aligned with OPC UA/Modbus design)
///
/// Uses ArcSwapOption for lock-free reads and atomic updates.
/// Supervisor owns the lifecycle and updates the client on connection/disconnection.
pub(super) struct ClientEntry {
    /// MQTT client (lock-free access via ArcSwapOption)
    pub client: ArcSwapOption<AsyncClient>,
    /// Health flag for fast-path checks
    pub healthy: AtomicBool,
    /// Shutdown flag set when supervisor stops
    pub shutdown: AtomicBool,
    /// Last error message for observability
    pub last_error: std::sync::Mutex<Option<String>>,
}

impl ClientEntry {
    /// Create a new empty client entry
    #[inline]
    pub fn new_empty() -> Self {
        Self {
            client: ArcSwapOption::from(None),
            healthy: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            last_error: std::sync::Mutex::new(None),
        }
    }

    /// Update the last error message
    ///
    /// This method is thread-safe and can be called from any context.
    /// It updates the last error message for observability and debugging.
    #[inline]
    pub fn update_error(&self, error: String) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error);
        }
    }

    /// Get the last error message
    ///
    /// Returns the most recent error message, if any.
    /// This method is thread-safe and can be called from any context.
    #[inline]
    #[allow(dead_code)] // Public API for observability/debugging
    pub fn get_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|guard| guard.clone())
    }

    /// Clear the last error message
    ///
    /// This method clears the stored error message, typically called
    /// when a successful connection is established.
    #[inline]
    pub fn clear_error(&self) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = None;
        }
    }
}

/// Shared pointer type for client entry
pub(super) type SharedClient = Arc<ClientEntry>;

/// ThingsBoard connection supervisor with auto-reconnect
///
/// Design aligned with driver supervisors (IEC104, S7, OPC UA, Modbus):
/// - Owns MQTT client lifecycle
/// - Drives event loop with exponential backoff reconnection
/// - Exposes watch channels for connection state and events
/// - Supports cancellation for clean shutdown
pub(super) struct ThingsBoardSupervisor {
    /// Configuration
    config: Arc<ThingsBoardPluginConfig>,
    /// Credentials for connection
    credentials: Arc<ProvisionCredentials>,
    /// Retry policy for reconnection
    retry_policy: RetryPolicy,
    /// Cancellation token
    cancel: CancellationToken,
    /// Connection state broadcaster
    state_tx: watch::Sender<NorthwardConnectionState>,
    /// Business events channel (RPC, Attribute updates, etc.)
    events_tx: mpsc::Sender<Event>,
    /// Shared client entry for lock-free access
    shared_client: SharedClient,
}

impl ThingsBoardSupervisor {
    /// Create a new supervisor
    pub fn new(
        config: Arc<ThingsBoardPluginConfig>,
        credentials: Arc<ProvisionCredentials>,
        retry_policy: RetryPolicy,
        cancel: CancellationToken,
        state_tx: watch::Sender<NorthwardConnectionState>,
        events_tx: mpsc::Sender<Event>,
        shared_client: SharedClient,
    ) -> Self {
        Self {
            config,
            credentials,
            retry_policy,
            cancel,
            state_tx,
            events_tx,
            shared_client,
        }
    }

    /// Run supervisor loop with auto-reconnect
    ///
    /// This method drives the MQTT connection lifecycle:
    /// 1. Attempt connection
    /// 2. Run event loop until disconnection
    /// 3. Apply exponential backoff
    /// 4. Retry based on policy (max_attempts semantics)
    ///
    /// # Max Attempts Semantics
    /// - `None` or `Some(0)`: Unlimited retries (use with caution!)
    /// - `Some(1)`: Only one attempt, fail immediately after first failure (no backoff)
    /// - `Some(n > 1)`: Retry up to n times total (including initial attempt)
    pub async fn run(self) {
        let config = Arc::clone(&self.config);
        let credentials = Arc::clone(&self.credentials);
        let retry_policy = self.retry_policy;
        let cancel = self.cancel.clone();
        let state_tx = self.state_tx.clone();
        let events_tx = self.events_tx.clone();
        let shared_client = Arc::clone(&self.shared_client);

        tokio::spawn(async move {
            let mut bo = build_exponential_backoff(&retry_policy);
            let mut attempt: u32 = 0;

            // Calculate max attempts: None or Some(0) = unlimited, Some(n) = n attempts total
            let should_retry = |current_attempt: u32| -> bool {
                match retry_policy.max_attempts {
                    None | Some(0) => true,             // Unlimited retries
                    Some(max) => current_attempt < max, // Retry up to max attempts
                }
            };

            loop {
                // Check if we should continue
                if cancel.is_cancelled() {
                    let _ = state_tx.send(NorthwardConnectionState::Failed(
                        "Supervisor cancelled".to_string(),
                    ));
                    info!("ThingsBoard supervisor cancelled");
                    break;
                }

                if !should_retry(attempt) {
                    let _ = state_tx.send(NorthwardConnectionState::Failed(format!(
                        "Max retry attempts ({:?}) exhausted",
                        retry_policy.max_attempts
                    )));
                    warn!(
                        max_attempts = ?retry_policy.max_attempts,
                        "ThingsBoard supervisor exhausted retry attempts"
                    );
                    break;
                }

                attempt += 1;
                info!(attempt, "ThingsBoard supervisor attempting connection");

                // Update state to Connecting
                let _ = state_tx.send(NorthwardConnectionState::Connecting);

                // Attempt to create MQTT client
                let (mqtt_client, event_loop) = match connect_mqtt_client(&config, &credentials) {
                    Ok(pair) => pair,
                    Err(e) => {
                        let error_msg = e.to_string();
                        warn!(error = %e, attempt, "Failed to create MQTT client");
                        shared_client.update_error(error_msg.clone());
                        let _ = state_tx.send(NorthwardConnectionState::Failed(error_msg.clone()));

                        // Check if we should retry before applying backoff
                        // If max_attempts = Some(1), we've already attempted once, so don't retry
                        if !should_retry(attempt) {
                            // No more retries allowed, return immediately without backoff
                            warn!(
                                max_attempts = ?retry_policy.max_attempts,
                                attempt,
                                "Max attempts reached, not retrying"
                            );
                            break;
                        }

                        // Apply backoff before retry
                        if let Some(delay) = bo.next_backoff() {
                            info!(
                                delay_ms = delay.as_millis() as u64,
                                "Backing off before retry"
                            );
                            tokio::select! {
                                _ = cancel.cancelled() => {
                                    info!("Backoff cancelled");
                                    break;
                                }
                                _ = tokio::time::sleep(delay) => {}
                            }
                        } else {
                            // Backoff exhausted (max_elapsed_time reached)
                            warn!("Exponential backoff exhausted");
                            let _ = state_tx.send(NorthwardConnectionState::Failed(
                                "Backoff time exhausted".to_string(),
                            ));
                            break;
                        }
                        continue;
                    }
                };

                // Run event loop until disconnection or cancellation
                let child = cancel.child_token();
                let seen_active = Self::run_event_loop(
                    mqtt_client,
                    event_loop,
                    Arc::clone(&config),
                    child.clone(),
                    state_tx.clone(),
                    events_tx.clone(),
                    Arc::clone(&shared_client),
                )
                .await;

                // If we successfully connected (seen_active), reset backoff
                if seen_active {
                    bo.reset();
                    attempt = 0; // Reset attempt counter on successful connection
                    info!("ThingsBoard connection was active, resetting backoff");
                }

                // Check cancellation before retrying
                if cancel.is_cancelled() {
                    info!("ThingsBoard supervisor cancelled after event loop");
                    break;
                }

                // Apply backoff before retry
                match bo.next_backoff() {
                    Some(delay) => {
                        let _ = state_tx.send(NorthwardConnectionState::Reconnecting);
                        info!(
                            attempt,
                            delay_ms = delay.as_millis() as u64,
                            "ThingsBoard reconnect backoff"
                        );
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                info!("Reconnect backoff cancelled");
                                break;
                            }
                            _ = tokio::time::sleep(delay) => {}
                        }
                    }
                    None => {
                        // Backoff exhausted
                        warn!("Exponential backoff exhausted during reconnect");
                        let _ = state_tx.send(NorthwardConnectionState::Failed(
                            "Backoff time exhausted".to_string(),
                        ));
                        break;
                    }
                }
            }

            info!("ThingsBoard supervisor loop terminated");
        });
    }

    /// Subscribe to required ThingsBoard topics
    ///
    /// This method subscribes to all necessary topics for ThingsBoard communication:
    /// - Device attributes (incoming)
    /// - Device attributes response (incoming)
    /// - Device RPC request (incoming)
    /// - Device RPC response (incoming)
    /// - Gateway attributes (incoming, if using gateway mode)
    /// - Gateway attributes response (incoming, if using gateway mode)
    /// - Gateway RPC (incoming, if using gateway mode)
    ///
    /// # Arguments
    /// * `client` - MQTT client for subscribing
    /// * `config` - Plugin configuration for QoS settings
    ///
    /// # Returns
    /// * `Ok(())` - All subscriptions successful
    /// * `Err(...)` - Subscription error
    async fn subscribe_required_topics(
        client: &AsyncClient,
        config: &ThingsBoardPluginConfig,
    ) -> NorthwardResult<()> {
        let qos = config.communication.qos();
        let topics = vec![
            // Device API topics (always required)
            (Topics::device_attributes(), qos),
            (Topics::device_attributes_response_sub(), qos),
            (Topics::device_rpc_request_sub(), qos),
            (Topics::device_rpc_response_sub(), qos),
            // Gateway API topics (required for gateway mode)
            (Topics::gateway_attributes(), qos),
            (Topics::gateway_attributes_response(), qos),
            (Topics::gateway_rpc(), qos),
        ];

        info!(
            topic_count = topics.len(),
            "Subscribing to required ThingsBoard topics"
        );

        for (topic, qos_level) in topics {
            client
                .subscribe(&topic, qos_level)
                .await
                .map_err(|e| NorthwardError::MqttError {
                    reason: format!("Failed to subscribe to topic '{}': {}", topic, e),
                })?;
            debug!(topic = %topic, qos = ?qos_level, "Subscribed to topic");
        }

        info!("Successfully subscribed to all required topics");
        Ok(())
    }

    /// Run MQTT event loop until disconnection or cancellation
    ///
    /// This method handles connection lifecycle events (ConnAck, Disconnect, PingResp)
    /// and forwards business events (Publish, SubAck, etc.) to the plugin for processing.
    ///
    /// # Arguments
    /// * `mqtt_client` - MQTT client for subscribing and disconnecting
    /// * `event_loop` - MQTT event loop for polling events
    /// * `config` - Plugin configuration for subscription settings
    /// * `cancel` - Cancellation token for graceful shutdown
    /// * `state_tx` - Connection state broadcaster
    /// * `events_tx` - Business events channel
    /// * `shared_client` - Shared client entry for lock-free access
    ///
    /// # Returns
    /// * `true` - Connection was successfully established (Active state seen)
    /// * `false` - Connection failed or was cancelled before establishment
    async fn run_event_loop(
        mqtt_client: AsyncClient,
        mut event_loop: EventLoop,
        config: Arc<ThingsBoardPluginConfig>,
        cancel: CancellationToken,
        state_tx: watch::Sender<NorthwardConnectionState>,
        events_tx: mpsc::Sender<Event>,
        shared_client: SharedClient,
    ) -> bool {
        let mut seen_active = false;
        let mut subscribed = false;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("Event loop cancelled, disconnecting");
                    // Clear client
                    shared_client.client.store(None);
                    shared_client.healthy.store(false, Ordering::Release);
                    let _ = state_tx.send(NorthwardConnectionState::Disconnected);
                    let _ = mqtt_client.disconnect().await;
                    break;
                }
                result = event_loop.poll() => {
                    match result {
                        Ok(event) => {
                            match &event {
                                Event::Incoming(Packet::ConnAck(_)) => {
                                    info!("MQTT connection established");
                                    seen_active = true;
                                    let _ = state_tx.send(NorthwardConnectionState::Connected);

                                    // Update shared client (lock-free, atomic)
                                    let client = Arc::new(mqtt_client.clone());
                                    shared_client.client.store(Some(client));
                                    shared_client.healthy.store(true, Ordering::Release);
                                    // Clear error on successful connection
                                    shared_client.clear_error();

                                    // Subscribe to required topics immediately after connection
                                    // This ensures we don't miss any messages
                                    if !subscribed {
                                        match Self::subscribe_required_topics(&mqtt_client, &config).await {
                                            Ok(()) => {
                                                subscribed = true;
                                                info!("Successfully subscribed to all required topics");
                                                // Clear error on successful subscription
                                                shared_client.clear_error();
                                            }
                                            Err(e) => {
                                                let error_msg = format!("Failed to subscribe to required topics: {}", e);
                                                warn!(error = %e, "Failed to subscribe to required topics");
                                                shared_client.update_error(error_msg);
                                                // Continue anyway - subscription might succeed later
                                                // But we should still try to process messages
                                            }
                                        }
                                    }
                                }
                                Event::Incoming(Packet::Disconnect) => {
                                    let error_msg = "MQTT server sent disconnect".to_string();
                                    info!("MQTT server sent disconnect");
                                    // Clear client
                                    shared_client.client.store(None);
                                    shared_client.healthy.store(false, Ordering::Release);
                                    shared_client.update_error(error_msg.clone());
                                    let _ = state_tx.send(NorthwardConnectionState::Disconnected);
                                    break;
                                }
                                Event::Incoming(Packet::PingResp) => {
                                    debug!("MQTT ping response received");
                                }
                                Event::Incoming(Packet::Publish(_)) => {
                                    // Forward business events (Publish) to plugin for processing
                                    if events_tx.send(event).await.is_err() {
                                        warn!("Events channel closed, terminating event loop");
                                        break;
                                    }
                                }
                                Event::Incoming(Packet::SubAck(_)) => {
                                    // Forward subscription acknowledgments to plugin
                                    if events_tx.send(event).await.is_err() {
                                        warn!("Events channel closed, terminating event loop");
                                        break;
                                    }
                                }
                                Event::Incoming(_) => {
                                    // Other incoming events (UnsubAck, etc.) are forwarded but typically not used
                                    debug!("Received other MQTT event: {:?}", event);
                                }
                                Event::Outgoing(_) => {
                                    // Outgoing events are just logging
                                    debug!("MQTT outgoing event: {:?}", event);
                                }
                            }
                        }
                        Err(e) => {
                            let error_msg = e.to_string();
                            warn!(error = %e, "MQTT event loop error");
                            // Clear client on error
                            shared_client.client.store(None);
                            shared_client.healthy.store(false, Ordering::Release);
                            shared_client.update_error(error_msg.clone());
                            let _ = state_tx.send(NorthwardConnectionState::Failed(error_msg));
                            break;
                        }
                    }
                }
            }
        }

        seen_active
    }
}
