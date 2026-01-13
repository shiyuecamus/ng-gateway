use super::{
    config::{ConnectionConfig, MessageFormat, ThingsBoardPluginConfig},
    handlers,
    provision::{
        load_or_prepare_credentials, store_credentials, ProvisionCredentials, ProvisionRequest,
        ProvisionResponse,
    },
    supervisor::{ClientEntry, SharedClient, ThingsBoardSupervisor},
    topics::Topics,
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    mqtt::router::{HandlerResult, MessageHandler, MessageRouter},
    ExtensionManager, NGValueJsonOptions, NorthwardConnectionState, NorthwardData, NorthwardError,
    NorthwardEvent, NorthwardInitContext, NorthwardResult, NorthwardRuntimeApi, Plugin,
    RetryPolicy,
};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde_json::{json, Map, Value};
use std::{
    future::Future,
    pin::Pin,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// ThingsBoard northward plugin
///
/// Design aligned with driver architecture:
/// - Supervisor manages MQTT connection lifecycle with auto-reconnect
/// - Plugin handles business logic (data publishing, RPC, attributes)
/// - Clean separation of concerns
pub struct ThingsBoardPlugin {
    /// Plugin configuration
    config: Arc<ThingsBoardPluginConfig>,

    /// Extension manager for persistent data
    extension_manager: Arc<dyn ExtensionManager>,

    /// Retry policy for connection supervisor
    retry_policy: RetryPolicy,

    /// App ID for logging
    app_id: i32,

    /// App name for logging
    app_name: String,

    /// Read-only runtime API for mapping attribute updates to point_ids.
    runtime: Arc<dyn NorthwardRuntimeApi>,

    /// Events channel for business events (RPC, Command, Attribute)
    events_tx: mpsc::Sender<NorthwardEvent>,

    /// Connection state broadcaster (internal)
    conn_state_tx: watch::Sender<NorthwardConnectionState>,
    conn_state_rx: watch::Receiver<NorthwardConnectionState>,

    /// Shared client entry for lock-free access (aligned with OPC UA/Modbus design)
    shared_client: SharedClient,

    /// Message router for handling MQTT messages
    router: Arc<MessageRouter>,

    /// Shutdown control
    shutdown_token: CancellationToken,
}

impl ThingsBoardPlugin {
    /// Construct a ThingsBoard plugin from initialization context (no I/O)
    pub fn with_ctx(ctx: NorthwardInitContext) -> NorthwardResult<Self> {
        let (conn_state_tx, conn_state_rx) = watch::channel(NorthwardConnectionState::Disconnected);
        let config = ctx
            .config
            .downcast_arc::<ThingsBoardPluginConfig>()
            .map_err(|_| NorthwardError::ConfigurationError {
                message: "Failed to downcast to ThingsBoardPluginConfig".to_string(),
            })?;
        Ok(Self {
            config,
            extension_manager: ctx.extension_manager,
            retry_policy: ctx.retry_policy,
            app_id: ctx.app_id,
            app_name: ctx.app_name,
            runtime: ctx.runtime,
            events_tx: ctx.events_tx,
            conn_state_tx,
            conn_state_rx,
            shared_client: Arc::new(ClientEntry::new_empty()),
            router: Arc::new(MessageRouter::new()),
            shutdown_token: CancellationToken::new(),
        })
    }

    /// Obtain credentials (load from storage or provision)
    async fn obtain_credentials(
        config: &Arc<ThingsBoardPluginConfig>,
        extension_manager: &Arc<dyn ExtensionManager>,
        app_id: i32,
        app_name: &str,
    ) -> NorthwardResult<ProvisionCredentials> {
        // Try to load existing credentials
        match load_or_prepare_credentials(&config.connection, extension_manager).await? {
            Some(creds) => {
                info!("Using existing credentials for app {}", app_id);
                Ok(creds)
            }
            None => {
                // Need to provision
                info!(
                    "No existing credentials, starting provision for app {}",
                    app_id
                );
                Self::perform_provision(config, extension_manager, app_id, app_name).await
            }
        }
    }

    /// Perform device provision using ThingsBoard API with retry logic
    ///
    /// Retry semantics:
    /// - `timeout_ms`: Total timeout for the entire provision process
    /// - `max_retries`: Maximum number of attempts (0 = infinite retries until timeout, 1 = only one attempt, fail immediately on failure)
    /// - `retry_delay_ms`: Delay between retry attempts (not applied if max_retries reached)
    async fn perform_provision(
        config: &ThingsBoardPluginConfig,
        extension_manager: &Arc<dyn ExtensionManager>,
        app_id: i32,
        app_name: &str,
    ) -> NorthwardResult<ProvisionCredentials> {
        // Validate provision config
        let (
            provision_device_key,
            provision_device_secret,
            provision_method,
            timeout_ms,
            max_retries,
            retry_delay_ms,
        ) = match &config.connection {
            ConnectionConfig::Provision {
                provision_device_key,
                provision_device_secret,
                provision_method,
                timeout_ms,
                max_retries,
                retry_delay_ms,
                ..
            } => (
                provision_device_key,
                provision_device_secret,
                provision_method,
                *timeout_ms,
                *max_retries,
                *retry_delay_ms,
            ),
            _ => {
                return Err(NorthwardError::ConfigurationError {
                    message: "Provision config not found".to_string(),
                })
            }
        };

        // Build provision request
        let request = ProvisionRequest::new(
            app_name.to_string(),
            provision_device_key.clone(),
            provision_device_secret.clone(),
            provision_method.clone(),
        );

        // Total timeout for entire provision process
        let total_timeout = Duration::from_millis(timeout_ms);
        let deadline = Instant::now() + total_timeout;
        let retry_delay = Duration::from_millis(retry_delay_ms);

        let mut attempt = 0u32;
        let mut last_error: Option<NorthwardError> = None;

        loop {
            // Check if we've exceeded total timeout
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(last_error.unwrap_or(NorthwardError::Timeout {
                    operation: "provision".to_string(),
                    timeout_ms,
                }));
            }

            attempt += 1;
            if attempt > 1 {
                info!(
                    "Retrying provision attempt {}/{} (remaining timeout: {:?})",
                    attempt,
                    if max_retries == 0 {
                        "∞".to_string()
                    } else {
                        max_retries.to_string()
                    },
                    remaining
                );
            } else {
                info!(
                    "Starting provision attempt {} (total timeout: {}ms, max_retries: {})",
                    attempt,
                    timeout_ms,
                    if max_retries == 0 {
                        "∞".to_string()
                    } else {
                        max_retries.to_string()
                    }
                );
            }

            // Attempt single provision request
            match Self::attempt_provision(
                config, &request, remaining, // Use remaining time for this attempt
            )
            .await
            {
                Ok(credentials) => {
                    // Success! Store credentials and return
                    info!("Provision succeeded on attempt {}", attempt);
                    store_credentials(&credentials, extension_manager).await?;
                    info!("Credentials stored for app {}", app_id);
                    return Ok(credentials);
                }
                Err(e) => {
                    last_error = Some(e);

                    // Check if we should retry before waiting
                    // max_retries = 0 means infinite retries
                    // max_retries = 1 means only one attempt, so if attempt == 1, don't retry
                    let should_retry =
                        max_retries == 0 || (max_retries > 0 && attempt < max_retries);

                    if !should_retry {
                        // No more retries allowed, return immediately without delay
                        warn!(attempt, max_retries, "Max retries reached, not retrying");
                        return Err(last_error.unwrap());
                    }

                    // If we have time left and retries available, wait and retry
                    let time_after_retry_delay = Instant::now() + retry_delay;
                    if time_after_retry_delay < deadline {
                        let delay_remaining =
                            retry_delay.min(deadline.saturating_duration_since(Instant::now()));
                        if !delay_remaining.is_zero() {
                            info!("Waiting {:?} before retry...", delay_remaining);
                            tokio::time::sleep(delay_remaining).await;
                        }
                        continue;
                    } else {
                        // No time left
                        return Err(last_error.unwrap());
                    }
                }
            }
        }
    }

    /// Attempt a single provision request
    ///
    /// This function performs one complete provision cycle:
    /// 1. Connect to MQTT broker
    /// 2. Subscribe to response topic
    /// 3. Publish provision request
    /// 4. Wait for response with timeout
    ///
    /// The event loop polling is handled in a background task to ensure
    /// it runs in the correct Tokio runtime context.
    async fn attempt_provision(
        config: &ThingsBoardPluginConfig,
        request: &ProvisionRequest,
        timeout: Duration,
    ) -> NorthwardResult<ProvisionCredentials> {
        let mut mqtt_options = MqttOptions::new("provision", config.host(), config.port());
        mqtt_options.set_keep_alive(std::time::Duration::from_secs(30));
        mqtt_options.set_clean_session(true);

        // Create client and event loop
        let (client, event_loop) = AsyncClient::new(mqtt_options, 10);

        // Subscribe to provision response topic
        let response_topic = "/provision/response";
        client
            .subscribe(response_topic, QoS::AtLeastOnce)
            .await
            .map_err(|e| NorthwardError::MqttError {
                reason: format!("Failed to subscribe to provision response: {}", e),
            })?;

        // Publish provision request
        let request_topic = "/provision/request";
        let payload =
            serde_json::to_vec(request).map_err(|e| NorthwardError::SerializationError {
                reason: e.to_string(),
            })?;

        client
            .publish(request_topic, QoS::AtLeastOnce, false, payload)
            .await
            .map_err(|e| NorthwardError::PublishFailed {
                platform: "thingsboard".to_string(),
                reason: e.to_string(),
            })?;

        debug!(
            "Provision request sent, waiting for response (timeout: {:?})",
            timeout
        );

        // Wait for provision response with timeout
        // Use a background task to poll event loop in the correct runtime context
        let (poll_tx, mut poll_rx) = mpsc::channel::<Result<Event, rumqttc::ConnectionError>>(10);

        // Move event loop to background task for polling
        let mut event_loop_for_task = event_loop;
        let poll_tx_for_task = poll_tx.clone();
        tokio::spawn(async move {
            // Poll event loop in background task (already in runtime context)
            loop {
                match event_loop_for_task.poll().await {
                    Ok(event) => {
                        if poll_tx_for_task.send(Ok(event)).await.is_err() {
                            // Channel closed, exit loop
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = poll_tx_for_task.send(Err(e)).await;
                        break;
                    }
                }
            }
        });

        let attempt_deadline = Instant::now() + timeout;
        let response: ProvisionResponse = loop {
            let remaining = attempt_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = client.disconnect().await;
                return Err(NorthwardError::Timeout {
                    operation: "provision attempt".to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            // Poll event loop with timeout using tokio::select!
            tokio::select! {
                result = poll_rx.recv() => {
                    match result {
                        Some(Ok(Event::Incoming(Packet::Publish(publish)))) => {
                            if response_topic == publish.topic {
                                match serde_json::from_slice::<ProvisionResponse>(&publish.payload) {
                                    Ok(resp) => break resp,
                                    Err(e) => {
                                        let _ = client.disconnect().await;
                                        return Err(NorthwardError::DeserializationError {
                                            reason: e.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                        Some(Ok(Event::Incoming(Packet::ConnAck(_)))) => {
                            // Connection acknowledged, continue polling
                            continue;
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => {
                            let _ = client.disconnect().await;
                            return Err(NorthwardError::MqttError {
                                reason: format!("Provision MQTT error: {}", e),
                            });
                        }
                        None => {
                            // Channel closed, event loop task terminated
                            let _ = client.disconnect().await;
                            return Err(NorthwardError::MqttError {
                                reason: "Event loop poll channel closed".to_string(),
                            });
                        }
                    }
                }
                _ = tokio::time::sleep(remaining) => {
                    let _ = client.disconnect().await;
                    return Err(NorthwardError::Timeout {
                        operation: "provision attempt".to_string(),
                        timeout_ms: timeout.as_millis() as u64,
                    });
                }
            }
        };

        // Disconnect provision client
        let _ = client.disconnect().await;

        debug!("Provision response received: {:?}", response.status);

        // Extract credentials from response
        response.extract_credentials()
    }

    /// Setup message routes for ThingsBoard topics
    async fn setup_routes(&self, events_tx: &mpsc::Sender<NorthwardEvent>) -> NorthwardResult<()> {
        // 1. Device attributes handler
        let device_attrs_handler: MessageHandler = {
            let events_tx = events_tx.clone();
            let runtime = Arc::clone(&self.runtime);
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let runtime = Arc::clone(&runtime);
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(async move {
                    handlers::handle_device_attributes(&topic, &payload, &events_tx, &runtime).await
                }) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(&Topics::device_attributes(), device_attrs_handler)
            .await;

        // 2. Device attributes response handler
        let device_attr_response_handler: MessageHandler = {
            let events_tx = events_tx.clone();
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(async move {
                    handlers::handle_device_attributes_response(&topic, &payload, &events_tx).await
                }) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(
                &Topics::device_attributes_response_sub(),
                device_attr_response_handler,
            )
            .await;

        // 3. Device RPC request handler
        let device_rpc_request_handler: MessageHandler = {
            let events_tx = events_tx.clone();
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(async move {
                    handlers::handle_device_rpc_request(&topic, &payload, &events_tx).await
                }) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(
                &Topics::device_rpc_request_sub(),
                device_rpc_request_handler,
            )
            .await;

        // 4. Device RPC response handler
        let device_rpc_response_handler: MessageHandler = {
            let events_tx = events_tx.clone();
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(async move {
                    handlers::handle_device_rpc_response(&topic, &payload, &events_tx).await
                }) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(
                &Topics::device_rpc_response_sub(),
                device_rpc_response_handler,
            )
            .await;

        // 7. Gateway RPC handler
        let gateway_rpc_handler: MessageHandler = {
            let events_tx = events_tx.clone();
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(
                    async move { handlers::handle_gateway_rpc(&topic, &payload, &events_tx).await },
                ) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(&Topics::gateway_rpc(), gateway_rpc_handler)
            .await;

        Ok(())
    }

    /// Handle MQTT events from supervisor using MessageRouter
    async fn event_loop_task(
        mut mqtt_events_rx: mpsc::Receiver<Event>,
        router: Arc<MessageRouter>,
        app_id: i32,
    ) {
        info!("Event loop task started for app {}", app_id);

        while let Some(event) = mqtt_events_rx.recv().await {
            match event {
                Event::Incoming(Packet::Publish(publish)) => {
                    let payload = &publish.payload[..];

                    debug!(app_id, topic = &publish.topic, "Received MQTT publish");

                    // Route message to appropriate handler
                    if let Err(e) = router.route_message(&publish.topic, payload).await {
                        tracing::warn!(app_id, error = %e, "Error handling MQTT message");
                    }
                }
                Event::Incoming(Packet::SubAck(_)) => {
                    debug!(app_id, "Subscription acknowledged");
                }
                Event::Incoming(Packet::ConnAck(_)) => {
                    debug!(app_id, "MQTT connection acknowledged in event loop");
                    // Notify router about connection
                    if let Err(e) = router.handle_connected().await {
                        tracing::warn!(app_id, error = %e, "Error handling connection event");
                    }
                }
                _ => {
                    // Other events are already handled by supervisor
                }
            }
        }

        info!("Event loop task terminated for app {}", app_id);
    }
}

#[async_trait]
impl Plugin for ThingsBoardPlugin {
    async fn start(&self) -> NorthwardResult<()> {
        let config = Arc::clone(&self.config);

        let extension_manager = Arc::clone(&self.extension_manager);

        let events_tx = self.events_tx.clone();

        info!(
            "Starting ThingsBoard plugin for app {} ({})",
            self.app_id, self.app_name
        );

        // Step 1: Obtain or provision credentials
        let credentials = Self::obtain_credentials(
            &config,
            &extension_manager,
            self.app_id,
            self.app_name.as_str(),
        )
        .await?;
        let credentials = Arc::new(credentials);

        // Step 2: Create MQTT events channel for business events (Publish, SubAck)
        let (mqtt_events_tx, mqtt_events_rx) = mpsc::channel(1000);

        // Step 3: Setup message routes on the internal router
        self.setup_routes(&events_tx).await?;

        // Step 5: Create and start supervisor with retry policy from context
        let supervisor = ThingsBoardSupervisor::new(
            config,
            credentials,
            self.retry_policy,
            self.shutdown_token.child_token(),
            self.conn_state_tx.clone(),
            mqtt_events_tx,
            Arc::clone(&self.shared_client),
        );

        supervisor.run().await;

        // Note: Client will be automatically updated by supervisor when connection is established
        // No need to wait for client here - it's available via ArcSwapOption

        // Step 6: Start event loop task to process business events using MessageRouter
        let router = Arc::clone(&self.router);
        tokio::spawn(Self::event_loop_task(mqtt_events_rx, router, self.app_id));

        info!(
            "ThingsBoard plugin started for app {} ({})",
            self.app_id, self.app_name
        );
        Ok(())
    }

    fn subscribe_connection_state(&self) -> watch::Receiver<NorthwardConnectionState> {
        self.conn_state_rx.clone()
    }

    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        // Lock-free read of client (atomic operation, no lock needed)
        let client = self
            .shared_client
            .client
            .load_full()
            .ok_or(NorthwardError::NotConnected)?;

        let config = Arc::clone(&self.config);

        // Build topic and payload based on data type
        let (topic, payload) = match config.communication.message_format {
            MessageFormat::Json => match data.as_ref() {
                NorthwardData::Telemetry(telemetry) => {
                    // ThingsBoard Gateway Telemetry JSON: { "Device": [ { "ts": ..., "values": {...} } ] }
                    let topic = Topics::gateway_telemetry();
                    let mut root = Map::with_capacity(1);
                    let mut values = Map::with_capacity(telemetry.values.len());
                    for pv in telemetry.values.iter() {
                        let key = pv.point_key.as_ref().to_string();
                        let v = pv.value.to_json_value(NGValueJsonOptions::default());
                        values.insert(key, v);
                    }
                    let entry = json!({
                        "ts": telemetry.timestamp.timestamp_millis(),
                        "values": Value::Object(values)
                    });
                    root.insert(telemetry.device_name.clone(), Value::Array(vec![entry]));
                    let bytes = serde_json::to_vec(&Value::Object(root)).map_err(|e| {
                        NorthwardError::SerializationError {
                            reason: e.to_string(),
                        }
                    })?;
                    (topic, bytes)
                }
                NorthwardData::Attributes(attributes) => {
                    // ThingsBoard Gateway Attributes JSON: { "Device": { "k":"v", ... } }
                    // Per TB docs this is client-side attributes publish API.
                    let topic = Topics::gateway_attributes();
                    let mut root = Map::with_capacity(1);
                    let mut device_attrs = Map::with_capacity(attributes.client_attributes.len());
                    for pv in attributes.client_attributes.iter() {
                        let key = pv.point_key.as_ref().to_string();
                        let v = pv.value.to_json_value(NGValueJsonOptions::default());
                        device_attrs.insert(key, v);
                    }
                    root.insert(attributes.device_name.clone(), Value::Object(device_attrs));
                    let bytes = serde_json::to_vec(&Value::Object(root)).map_err(|e| {
                        NorthwardError::SerializationError {
                            reason: e.to_string(),
                        }
                    })?;
                    (topic, bytes)
                }
                NorthwardData::DeviceConnected(device_connected) => {
                    // ThingsBoard Gateway Device Connected JSON: { "Device": { "k":"v", ... } }
                    let topic = Topics::gateway_connect();
                    let bytes = serde_json::to_vec(&json!({
                        "device": device_connected.device_name,
                        "type": device_connected.device_type
                    }))
                    .map_err(|e| NorthwardError::SerializationError {
                        reason: e.to_string(),
                    })?;
                    (topic, bytes)
                }
                NorthwardData::DeviceDisconnected(device_disconnected) => {
                    // ThingsBoard Gateway Device Disconnected JSON: { "Device": { "k":"v", ... } }
                    let topic = Topics::gateway_disconnect();
                    let bytes = serde_json::to_vec(&json!({
                        "device": device_disconnected.device_name,
                        "type": device_disconnected.device_type
                    }))
                    .map_err(|e| NorthwardError::SerializationError {
                        reason: e.to_string(),
                    })?;
                    (topic, bytes)
                }
                _ => {
                    warn!(app_id = self.app_id, "Unsupported data type: {:?}", data);
                    return Ok(());
                }
            },
            MessageFormat::Protobuf => {
                // TODO: Implement protobuf serialization
                return Err(NorthwardError::InvalidMessageFormat {
                    reason: "Protobuf format not yet implemented".to_string(),
                });
            }
        };
        // Publish to ThingsBoard
        client
            .publish(topic, config.communication.qos(), false, payload)
            .await
            .map_err(|e| NorthwardError::PublishFailed {
                platform: "thingsboard".to_string(),
                reason: e.to_string(),
            })?;

        Ok(())
    }

    async fn stop(&self) -> NorthwardResult<()> {
        info!("Stopping ThingsBoard plugin");

        // Signal shutdown
        self.shutdown_token.cancel();

        // Mark shutdown and clear client
        self.shared_client.shutdown.store(true, Ordering::Release);
        self.shared_client.client.store(None);
        self.shared_client.healthy.store(false, Ordering::Release);

        info!(
            "ThingsBoard plugin stopped: {} ({})",
            self.app_id, self.app_name
        );
        Ok(())
    }
}
