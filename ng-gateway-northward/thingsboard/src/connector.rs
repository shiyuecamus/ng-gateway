//! ThingsBoard supervised connector implementation.
//!
//! This module provides the final-form integration for ThingsBoard:
//! - `ThingsBoardConnector`: implements SDK `Connector` and is constructed via `ThingsBoardConnector::new(ctx)`.
//! - `connect()`: performs credential loading/provisioning and creates an MQTT client + event loop.

use super::{
    config::{ConnectionConfig, ThingsBoardPluginConfig},
    handle::ThingsBoardHandle,
    handlers::{
        handle_device_attributes_response, handle_device_rpc_request, handle_device_rpc_response,
        handle_gateway_device_attributes, handle_gateway_rpc, handle_sub_device_attributes,
    },
    mqtt::connect_mqtt_client,
    provision::{
        load_or_prepare_credentials, store_credentials, ProvisionCredentials, ProvisionRequest,
        ProvisionResponse,
    },
    session::ThingsBoardSession,
    topics::Topics,
};
use ng_gateway_sdk::{
    mqtt::router::{HandlerResult, MessageHandler, MessageRouter},
    supervision::{Connector, Session, SessionContext},
    ExtensionStore, FailureKind, FailurePhase, NorthwardError, NorthwardEvent,
    NorthwardInitContext, NorthwardResult, NorthwardRuntimeApi,
};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// ThingsBoard supervised connector (constructed from init context, no I/O).
#[derive(Clone)]
pub struct ThingsBoardConnector {
    config: Arc<ThingsBoardPluginConfig>,
    extension_store: Arc<dyn ExtensionStore>,
    app_id: i32,
    app_name: String,
    runtime: Arc<dyn NorthwardRuntimeApi>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    router: Arc<MessageRouter>,
}

impl ThingsBoardConnector {
    /// Create the connector from init context (no I/O).
    pub fn from_init(ctx: NorthwardInitContext) -> NorthwardResult<Self> {
        let config = ctx
            .config
            .downcast_arc::<ThingsBoardPluginConfig>()
            .map_err(|_| NorthwardError::ConfigurationError {
                message: "Failed to downcast to ThingsBoardPluginConfig".to_string(),
            })?;

        Ok(Self {
            config,
            extension_store: ctx.extension_store,
            app_id: ctx.app_id,
            app_name: ctx.app_name,
            runtime: ctx.runtime,
            events_tx: ctx.events_tx,
            router: Arc::new(MessageRouter::new()),
        })
    }

    async fn setup_routes(&self) -> NorthwardResult<()> {
        // 1. Sub-device shared attributes changed (Gateway API)
        let sub_device_attrs_handler: MessageHandler = {
            let events_tx = self.events_tx.clone();
            let runtime = Arc::clone(&self.runtime);
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let runtime = Arc::clone(&runtime);
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(async move {
                    handle_sub_device_attributes(&topic, &payload, &events_tx, &runtime).await
                }) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(&Topics::gateway_attributes(), sub_device_attrs_handler)
            .await;

        // 2. Gateway device attributes changed (Device API) - observability only
        let gateway_device_attrs_handler: MessageHandler =
            Arc::new(move |topic: &str, payload: &[u8]| {
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(async move {
                    // NOTE: This is intentionally log-only for now.
                    handle_gateway_device_attributes(&topic, &payload).await
                }) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            });
        self.router
            .register(&Topics::device_attributes(), gateway_device_attrs_handler)
            .await;

        // 3. Device attributes response handler
        let device_attr_response_handler: MessageHandler = {
            let events_tx = self.events_tx.clone();
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(async move {
                    handle_device_attributes_response(&topic, &payload, &events_tx).await
                }) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(
                &Topics::device_attributes_response_sub(),
                device_attr_response_handler,
            )
            .await;

        // 4. Device RPC request handler
        let device_rpc_request_handler: MessageHandler = {
            let events_tx = self.events_tx.clone();
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(
                    async move { handle_device_rpc_request(&topic, &payload, &events_tx).await },
                ) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(
                &Topics::device_rpc_request_sub(),
                device_rpc_request_handler,
            )
            .await;

        // 5. Device RPC response handler
        let device_rpc_response_handler: MessageHandler = {
            let events_tx = self.events_tx.clone();
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(
                    async move { handle_device_rpc_response(&topic, &payload, &events_tx).await },
                ) as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(
                &Topics::device_rpc_response_sub(),
                device_rpc_response_handler,
            )
            .await;

        // 6. Gateway RPC handler
        let gateway_rpc_handler: MessageHandler = {
            let events_tx = self.events_tx.clone();
            Arc::new(move |topic: &str, payload: &[u8]| {
                let events_tx = events_tx.clone();
                let topic = topic.to_string();
                let payload = payload.to_vec();
                Box::pin(async move { handle_gateway_rpc(&topic, &payload, &events_tx).await })
                    as Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
            })
        };
        self.router
            .register(&Topics::gateway_rpc(), gateway_rpc_handler)
            .await;

        Ok(())
    }

    async fn obtain_credentials(
        &self,
        ctx: &SessionContext,
    ) -> NorthwardResult<ProvisionCredentials> {
        match load_or_prepare_credentials(&self.config.connection, &self.extension_store).await? {
            Some(creds) => Ok(creds),
            None => {
                // Provision mode but no stored credentials.
                let creds = self.perform_provision(ctx).await?;
                store_credentials(&creds, &self.extension_store).await?;
                Ok(creds)
            }
        }
    }

    async fn perform_provision(
        &self,
        ctx: &SessionContext,
    ) -> NorthwardResult<ProvisionCredentials> {
        let (
            provision_device_key,
            provision_device_secret,
            provision_method,
            timeout_ms,
            max_retries,
            retry_delay_ms,
        ) = match &self.config.connection {
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
                });
            }
        };

        let request = ProvisionRequest::new(
            self.app_name.clone(),
            provision_device_key.clone(),
            provision_device_secret.clone(),
            provision_method.clone(),
        );

        let total_timeout = Duration::from_millis(timeout_ms);
        let deadline = Instant::now() + total_timeout;
        let retry_delay = Duration::from_millis(retry_delay_ms);

        let mut attempt = 0u32;
        loop {
            if ctx.cancel.is_cancelled() {
                return Err(NorthwardError::NotConnected);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(NorthwardError::Timeout {
                    operation: "provision".to_string(),
                    timeout_ms,
                });
            }

            attempt += 1;
            if max_retries != 0 && attempt > max_retries {
                return Err(NorthwardError::ProvisionFailed {
                    platform: "thingsboard".to_string(),
                    reason: "max retries reached".to_string(),
                });
            }

            let provision_res = tokio::select! {
                _ = ctx.cancel.cancelled() => Err(NorthwardError::NotConnected),
                res = tokio::time::timeout(remaining, self.provision_once(ctx, &request)) => {
                    match res {
                        Ok(inner) => inner,
                        Err(_) => Err(NorthwardError::Timeout { operation: "provision".to_string(), timeout_ms }),
                    }
                }
            };

            match provision_res {
                Ok(creds) => return Ok(creds),
                Err(e) => {
                    warn!(app_id = self.app_id, error = %e, "Provision attempt failed");
                }
            }

            tokio::select! {
                _ = ctx.cancel.cancelled() => return Err(NorthwardError::NotConnected),
                _ = tokio::time::sleep(retry_delay) => {}
            }
        }
    }

    async fn provision_once(
        &self,
        ctx: &SessionContext,
        request: &ProvisionRequest,
    ) -> NorthwardResult<ProvisionCredentials> {
        let payload =
            serde_json::to_vec(request).map_err(|e| NorthwardError::SerializationError {
                reason: e.to_string(),
            })?;

        // IMPORTANT:
        // Provision uses a short-lived, isolated MQTT client with:
        // - client_id = "provision"
        // - clean_session = true
        // - keep_alive = 30s
        // This mirrors the legacy behavior and avoids coupling provision traffic
        // with the long-lived gateway session.
        let (client, mut event_loop) = create_provision_client(&self.config)?;

        // Subscribe to the provision response topic before sending the request.
        client
            .subscribe(&Topics::device_provision_response(), QoS::AtLeastOnce)
            .await
            .map_err(|e| NorthwardError::MqttError {
                reason: format!("Failed to subscribe to provision response: {e}"),
            })?;

        client
            .publish(
                &Topics::device_provision_request(),
                QoS::AtLeastOnce,
                false,
                payload,
            )
            .await
            .map_err(|e| NorthwardError::MqttError {
                reason: e.to_string(),
            })?;

        // Wait for a response publish on the provision response topic.
        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    let _ = client.disconnect().await;
                    return Err(NorthwardError::NotConnected);
                }
                res = event_loop.poll() => {
                    match res {
                        Ok(Event::Incoming(Packet::Publish(publish))) => {
                            // Only accept messages from the expected provision response topic.
                            if publish.topic != Topics::device_provision_response() {
                                continue;
                            }

                            let resp: ProvisionResponse =
                                serde_json::from_slice(&publish.payload).map_err(|e| {
                                    NorthwardError::DeserializationError {
                                        reason: e.to_string(),
                                    }
                                })?;

                            let creds = resp.extract_credentials()?;
                            let _ = client.disconnect().await;
                            return Ok(creds);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            let _ = client.disconnect().await;
                            return Err(NorthwardError::MqttError { reason: e.to_string() });
                        }
                    }
                }
            }
        }
    }
}

/// Create a short-lived MQTT client for provisioning.
///
/// This MUST stay aligned with legacy behavior:
/// - `client_id = "provision"`
/// - `keep_alive = 30s`
/// - `clean_session = true`
fn create_provision_client(
    config: &ThingsBoardPluginConfig,
) -> NorthwardResult<(AsyncClient, EventLoop)> {
    let mut mqtt_options = MqttOptions::new("provision", config.host(), config.port());
    mqtt_options.set_keep_alive(Duration::from_secs(30));
    mqtt_options.set_clean_session(true);

    let (client, event_loop) = AsyncClient::new(mqtt_options, 10);
    Ok((client, event_loop))
}

#[async_trait::async_trait]
impl Connector for ThingsBoardConnector {
    type InitContext = NorthwardInitContext;
    type Handle = ThingsBoardHandle;
    type Session = ThingsBoardSession;

    #[inline]
    fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        Self::from_init(ctx)
    }

    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        self.setup_routes().await?;

        info!(
            app_id = self.app_id,
            "ThingsBoard connect: obtaining credentials"
        );
        let credentials = self.obtain_credentials(&ctx).await?;
        let (client, event_loop) = connect_mqtt_client(&self.config, &credentials)?;

        Ok(ThingsBoardSession {
            handle: Arc::new(ThingsBoardHandle::new(Arc::clone(&self.config), client)),
            config: Arc::clone(&self.config),
            router: Arc::clone(&self.router),
            event_loop: Some(event_loop),
            app_id: self.app_id,
        })
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        match err {
            NorthwardError::ConfigurationError { .. } => FailureKind::Fatal,
            _ => FailureKind::Retryable,
        }
    }
}
