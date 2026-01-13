//! Kafka northward plugin implementation.
//!
//! Design goals (aligned with `ng-plugin-pulsar`):
//! - Keep `process_data()` CPU-only and non-blocking.
//! - Centralize Kafka I/O into a dedicated publisher task.
//! - Use bounded queues for backpressure and to prevent unbounded memory growth.
//! - Supervisor owns connection/reconnect logic and connection state.

use super::{
    config::{DownlinkConfig, EventUplink, KafkaPluginConfig},
    supervisor::{ClientEntry, KafkaSupervisor, KafkaSupervisorParams, SharedClient},
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
use rdkafka::{
    message::{Header, OwnedHeaders},
    producer::FutureRecord,
};
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
    key: Option<String>,
    headers: HashMap<String, String>,
    ts_ms: Option<i64>,
}

#[derive(Debug, Clone)]
struct OutboundPublish {
    topic: String,
    msg: OutboundMessage,
}

/// Kafka northward plugin (uplink + downlink).
pub struct KafkaPlugin {
    config: Arc<KafkaPluginConfig>,
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

impl KafkaPlugin {
    /// Construct plugin instance from initialization context (no I/O).
    pub fn with_ctx(ctx: NorthwardInitContext) -> NorthwardResult<Self> {
        let (conn_state_tx, conn_state_rx) = watch::channel(NorthwardConnectionState::Disconnected);
        let config = ctx
            .config
            .downcast_arc::<KafkaPluginConfig>()
            .map_err(|_| NorthwardError::ConfigurationError {
                message: "Failed to downcast to KafkaPluginConfig".to_string(),
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
impl Plugin for KafkaPlugin {
    async fn start(&self) -> NorthwardResult<()> {
        info!(
            app_id = self.app_id,
            app_name = %self.app_name,
            "Starting Kafka plugin"
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

        let supervisor = KafkaSupervisor::new(KafkaSupervisorParams {
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

        info!(app_id = self.app_id, "Kafka plugin started");
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
            "kafka", // plugin_type
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

        let ts_ms = ctx.ts.timestamp_millis();
        let headers: HashMap<String, String> = ctx.into();

        // Do not perform any Kafka I/O on AppActor critical path.
        self.outbound_tx
            .try_send(OutboundPublish {
                topic,
                msg: OutboundMessage {
                    payload,
                    key,
                    headers,
                    ts_ms: Some(ts_ms),
                },
            })
            .map_err(|e| NorthwardError::PublishFailed {
                platform: "kafka".to_string(),
                reason: format!("outbound queue rejected: {e}"),
            })
    }

    async fn stop(&self) -> NorthwardResult<()> {
        info!(app_id = self.app_id, "Stopping Kafka plugin");

        self.shutdown_token.cancel();

        self.shared_client.shutdown.store(true, Ordering::Release);
        self.shared_client.producer.store(None);
        self.shared_client.healthy.store(false, Ordering::Release);

        info!(app_id = self.app_id, "Kafka plugin stopped");
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
    /// Publisher task owns the send-path side effects (Kafka I/O + delivery receipts).
    ///
    /// Design goals:
    /// - Keep `process_data()` CPU-only and non-blocking for high throughput.
    /// - Centralize error handling / reconnect triggers.
    /// - Bound in-flight delivery awaits to avoid unbounded task growth.
    const MAX_INFLIGHT_DELIVERIES: usize = 1024;

    tokio::spawn(async move {
        let mut inflight = JoinSet::new();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                Some(p) = rx.recv() => {
                    // Throttle delivery tasks to avoid unbounded memory growth.
                    while inflight.len() >= MAX_INFLIGHT_DELIVERIES {
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

                    let headers = build_headers(&p.msg.headers);
                    let mut record = FutureRecord::to(p.topic.as_str()).payload(&p.msg.payload);
                    if let Some(key) = p.msg.key.as_ref() {
                        record = record.key(key.as_str());
                    }
                    if let Some(ts_ms) = p.msg.ts_ms {
                        // Kafka expects millisecond timestamps.
                        record = record.timestamp(ts_ms);
                    }
                    record = record.headers(headers);

                    // Non-blocking produce; delivery is awaited in a bounded task set.
                    match producer.send_result(record) {
                        Ok(delivery_f) => {
                            let reconnect_tx = reconnect_tx.clone();
                            let topic = p.topic;
                            inflight.spawn(async move {
                                match delivery_f.await {
                                    Ok(Ok(_delivery)) => {
                                        debug!(app_id, topic=%topic, "kafka delivery ok");
                                    }
                                    Ok(Err((e, _msg))) => {
                                        warn!(app_id, topic=%topic, error=%e, "kafka delivery failed");
                                        let _ = reconnect_tx.try_send(e.to_string());
                                    }
                                    Err(e) => {
                                        warn!(app_id, topic=%topic, error=%e, "kafka delivery future cancelled");
                                        let _ = reconnect_tx.try_send(format!("delivery future cancelled: {e}"));
                                    }
                                }
                            });
                        }
                        Err((e, _owned_msg)) => {
                            // QueueFull is a normal backpressure condition; do not trigger reconnect.
                            warn!(app_id, topic=%p.topic, error=%e, "kafka send_result failed");
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

#[inline]
fn build_headers(map: &HashMap<String, String>) -> OwnedHeaders {
    let mut headers = OwnedHeaders::new();
    for (k, v) in map {
        headers = headers.insert(Header {
            key: k.as_str(),
            value: Some(v.as_bytes()),
        });
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DownlinkConfig;
    use ng_gateway_sdk::northward::downlink::AckPolicy;

    #[test]
    fn test_build_downlink_routes_disabled() {
        let cfg = DownlinkConfig {
            enabled: false,
            ..Default::default()
        };
        let table = build_downlink_routes(&cfg).expect("build routes");
        assert!(table.is_none());
    }

    #[test]
    fn test_build_downlink_routes_enabled_one_topic() {
        let mut cfg = DownlinkConfig {
            enabled: true,
            ..Default::default()
        };
        cfg.write_point.enabled = true;
        cfg.write_point.topic = "ng.downlink".to_string();
        cfg.write_point.ack_policy = AckPolicy::OnSuccess;

        let table = build_downlink_routes(&cfg).expect("build routes");
        let table = table.expect("table exists");
        assert_eq!(table.topics.len(), 1);
        assert_eq!(table.topics[0], "ng.downlink");
    }
}
