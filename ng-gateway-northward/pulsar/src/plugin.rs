use super::{
    config::{DownlinkConfig, EventUplink, PulsarPluginConfig},
    supervisor::{ClientEntry, PulsarSupervisor, PulsarSupervisorParams, SharedClient},
};
use async_trait::async_trait;
use ng_gateway_sdk::northward::{
    downlink::{build_route_table, DownlinkKind, DownlinkRoute, DownlinkRouteTable},
    payload::{build_context, encode_uplink_payload, UplinkEventKind},
    template::render_template,
};
use ng_gateway_sdk::{
    ExtensionManager, NorthwardConnectionState, NorthwardData, NorthwardError, NorthwardEvent,
    NorthwardInitContext, NorthwardResult, NorthwardRuntimeApi, Plugin, RetryPolicy,
};
use pulsar::{producer, SerializeMessage};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc},
};
use tokio::{
    sync::{mpsc, watch, Mutex},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
struct OutboundMessage {
    payload: Vec<u8>,
    partition_key: Option<String>,
    properties: HashMap<String, String>,
    event_time: Option<u64>,
}

#[derive(Debug, Clone)]
struct OutboundPublish {
    topic: String,
    msg: OutboundMessage,
}

impl SerializeMessage for OutboundMessage {
    fn serialize_message(input: Self) -> Result<producer::Message, pulsar::Error> {
        Ok(producer::Message {
            payload: input.payload,
            properties: input.properties,
            partition_key: input.partition_key,
            event_time: input.event_time,
            ..Default::default()
        })
    }
}

/// Pulsar northward plugin (Phase 1: uplink only).
pub struct PulsarPlugin {
    config: Arc<PulsarPluginConfig>,
    _extension_manager: Arc<dyn ExtensionManager>,
    retry_policy: RetryPolicy,
    app_id: i32,
    app_name: String,
    runtime: Arc<dyn NorthwardRuntimeApi>,
    events_tx: mpsc::Sender<NorthwardEvent>,

    conn_state_tx: watch::Sender<NorthwardConnectionState>,
    conn_state_rx: watch::Receiver<NorthwardConnectionState>,

    shared_client: SharedClient,
    reconnect_tx: mpsc::Sender<String>,
    reconnect_rx: Mutex<Option<mpsc::Receiver<String>>>,
    outbound_tx: mpsc::Sender<OutboundPublish>,
    outbound_rx: Mutex<Option<mpsc::Receiver<OutboundPublish>>>,
    shutdown_token: CancellationToken,
}

impl PulsarPlugin {
    pub fn with_ctx(ctx: NorthwardInitContext) -> NorthwardResult<Self> {
        let (conn_state_tx, conn_state_rx) = watch::channel(NorthwardConnectionState::Disconnected);
        let config = ctx
            .config
            .downcast_arc::<PulsarPluginConfig>()
            .map_err(|_| NorthwardError::ConfigurationError {
                message: "Failed to downcast to PulsarPluginConfig".to_string(),
            })?;

        // reconnect channel is only used as a best-effort signal
        let (reconnect_tx, reconnect_rx) = mpsc::channel(32);

        // App-level outbound queue for structured concurrency publisher task.
        // Keep this bounded to apply backpressure and avoid unbounded memory growth.
        //
        // NOTE: This is intentionally a plugin-internal default (not exposed in config).
        let outbound_capacity = 100usize;
        let (outbound_tx, outbound_rx) = mpsc::channel(outbound_capacity);

        Ok(Self {
            config,
            _extension_manager: ctx.extension_manager,
            retry_policy: ctx.retry_policy,
            app_id: ctx.app_id,
            app_name: ctx.app_name,
            runtime: ctx.runtime,
            events_tx: ctx.events_tx,
            conn_state_tx,
            conn_state_rx,
            shared_client: Arc::new(ClientEntry::new_empty()),
            reconnect_tx,
            reconnect_rx: Mutex::new(Some(reconnect_rx)),
            outbound_tx,
            outbound_rx: Mutex::new(Some(outbound_rx)),
            shutdown_token: CancellationToken::new(),
        })
    }

    fn select_uplink_mapping(
        &self,
        data: &NorthwardData,
    ) -> Option<(UplinkEventKind, &EventUplink)> {
        match data {
            NorthwardData::DeviceConnected(_) => Some((
                UplinkEventKind::DeviceConnected,
                &self.config.uplink.device_connected,
            )),
            NorthwardData::DeviceDisconnected(_) => Some((
                UplinkEventKind::DeviceDisconnected,
                &self.config.uplink.device_disconnected,
            )),
            NorthwardData::Telemetry(_) => {
                Some((UplinkEventKind::Telemetry, &self.config.uplink.telemetry))
            }
            NorthwardData::Attributes(_) => {
                Some((UplinkEventKind::Attributes, &self.config.uplink.attributes))
            }
            _ => None,
        }
    }
}

#[async_trait]
impl Plugin for PulsarPlugin {
    async fn start(&self) -> NorthwardResult<()> {
        info!(
            app_id = self.app_id,
            app_name = %self.app_name,
            "Starting Pulsar plugin"
        );

        let reconnect_rx =
            self.reconnect_rx
                .lock()
                .await
                .take()
                .ok_or(NorthwardError::ConfigurationError {
                    message: "Reconnect channel already taken (plugin started twice)".to_string(),
                })?;

        let outbound_rx =
            self.outbound_rx
                .lock()
                .await
                .take()
                .ok_or(NorthwardError::ConfigurationError {
                    message: "Outbound channel already taken (plugin started twice)".to_string(),
                })?;

        spawn_publisher_task(
            self.app_id,
            outbound_rx,
            Arc::clone(&self.shared_client),
            self.reconnect_tx.clone(),
            self.shutdown_token.child_token(),
        );

        let downlink = build_downlink_routes(&self.config.downlink)
            .map_err(|e| NorthwardError::ConfigurationError { message: e })?;

        let supervisor = PulsarSupervisor::new(PulsarSupervisorParams {
            app_id: self.app_id,
            conn: self.config.connection.clone(),
            producer_cfg: self.config.uplink.producer.clone(),
            downlink_routes: downlink,
            retry_policy: self.retry_policy,
            cancel: self.shutdown_token.child_token(),
            state_tx: self.conn_state_tx.clone(),
            shared_client: Arc::clone(&self.shared_client),
            reconnect_tx: self.reconnect_tx.clone(),
            events_tx: self.events_tx.clone(),
            reconnect_rx,
        });
        supervisor.run().await;

        info!(app_id = self.app_id, "Pulsar plugin started");
        Ok(())
    }

    fn subscribe_connection_state(&self) -> watch::Receiver<NorthwardConnectionState> {
        self.conn_state_rx.clone()
    }

    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        if !self.config.uplink.enabled {
            return Ok(());
        }
        let Some((event_kind, mapping)) = self.select_uplink_mapping(data.as_ref()) else {
            return Ok(());
        };
        if !mapping.enabled {
            return Ok(());
        }

        let Some(ctx) = build_context(
            self.app_id,
            &self.app_name,
            "pulsar", // plugin_type
            event_kind,
            data.as_ref(),
            &self.runtime,
        ) else {
            return Ok(());
        };

        let tmpl_data = serde_json::to_value(&ctx).unwrap_or(Value::Null);
        let topic = render_template(mapping.topic.as_str(), &tmpl_data);
        let key_rendered = render_template(mapping.key.as_str(), &tmpl_data);
        let key = if key_rendered.trim().is_empty() {
            None
        } else {
            Some(key_rendered)
        };

        let payload = encode_uplink_payload(&mapping.payload, &ctx, data.as_ref(), &self.runtime)
            .map_err(|e| NorthwardError::SerializationError {
            reason: e.to_string(),
        })?;

        let ts = ctx.ts.timestamp_millis() as u64;
        let msg = OutboundMessage {
            payload,
            partition_key: key,
            properties: ctx.into(),
            event_time: Some(ts),
        };

        // Do not perform any Pulsar I/O on AppActor critical path.
        self.outbound_tx
            .try_send(OutboundPublish { topic, msg })
            .map_err(|e| NorthwardError::PublishFailed {
                platform: "pulsar".to_string(),
                reason: format!("outbound queue rejected: {e}"),
            })
    }

    async fn stop(&self) -> NorthwardResult<()> {
        info!(app_id = self.app_id, "Stopping Pulsar plugin");

        self.shutdown_token.cancel();

        self.shared_client.shutdown.store(true, Ordering::Release);
        self.shared_client.producer.store(None);
        self.shared_client.healthy.store(false, Ordering::Release);

        info!(app_id = self.app_id, "Pulsar plugin stopped");
        Ok(())
    }
}

fn build_downlink_routes(cfg: &DownlinkConfig) -> Result<Option<Arc<DownlinkRouteTable>>, String> {
    if !cfg.enabled {
        return Ok(None);
    }

    let mut routes = Vec::new();
    if cfg.write_point.enabled {
        routes.push(DownlinkRoute {
            kind: DownlinkKind::WritePoint,
            mapping: cfg.write_point.clone(),
        });
    }
    if cfg.command_received.enabled {
        routes.push(DownlinkRoute {
            kind: DownlinkKind::CommandReceived,
            mapping: cfg.command_received.clone(),
        });
    }
    if cfg.rpc_response_received.enabled {
        routes.push(DownlinkRoute {
            kind: DownlinkKind::RpcResponseReceived,
            mapping: cfg.rpc_response_received.clone(),
        });
    }

    if routes.is_empty() {
        return Ok(None);
    }

    let table = build_route_table(routes)?;
    Ok(Some(Arc::new(table)))
}

fn spawn_publisher_task(
    app_id: i32,
    mut rx: mpsc::Receiver<OutboundPublish>,
    shared_client: SharedClient,
    reconnect_tx: mpsc::Sender<String>,
    cancel: CancellationToken,
) {
    /// Publisher task owns the send-path side effects (Pulsar I/O + receipts).
    ///
    /// Design goals:
    /// - Keep `process_data()` CPU-only and non-blocking for high throughput.
    /// - Centralize error handling / reconnect triggers.
    /// - Bound in-flight receipt awaits to avoid unbounded task growth.
    const MAX_INFLIGHT_RECEIPTS: usize = 1024;

    tokio::spawn(async move {
        let mut inflight = JoinSet::new();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                Some(p) = rx.recv() => {
                    // Throttle receipt tasks to avoid unbounded memory growth.
                    while inflight.len() >= MAX_INFLIGHT_RECEIPTS {
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = inflight.join_next() => {}
                        }
                    }

                    // Wait until connected (producer available). This is the controlled backpressure point.
                    let producer = loop {
                        if cancel.is_cancelled() {
                            break None;
                        }
                        if let Some(p) = shared_client.producer.load_full() {
                            break Some(p);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    };
                    let Some(producer) = producer else { break; };

                    // Only this task uses the producer, so the mutex is uncontended and off the hot path.
                    let send_f = {
                        let mut locked = producer.lock().await;
                        locked.send_non_blocking(p.topic.clone(), p.msg).await
                    };

                    match send_f {
                        Ok(receipt_f) => {
                            let reconnect_tx = reconnect_tx.clone();
                            let topic = p.topic;
                            inflight.spawn(async move {
                                match receipt_f.await {
                                    Ok(_receipt) => {
                                        debug!(app_id, topic=%topic, "pulsar send receipt ok");
                                    }
                                    Err(e) => {
                                        warn!(app_id, topic=%topic, error=%e, "pulsar send receipt failed");
                                        let _ = reconnect_tx.try_send(e.to_string());
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            warn!(app_id, topic=%p.topic, error=%e, "pulsar send_non_blocking failed");
                            let _ = reconnect_tx.try_send(e.to_string());
                        }
                    }
                }
            }
        }

        // Best-effort drain inflight tasks on shutdown.
        while inflight.join_next().await.is_some() {}
    });
}
