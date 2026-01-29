//! Kafka supervised session implementation.
//!
//! This module contains the per-attempt session lifecycle driven by the SDK supervisor:
//! - `init()`: defines "Ready" (producer is already created in `connect()`).
//! - `run()`: drives publisher (uplink) and optional downlink consumer loop.

use super::{
    config::{
        KafkaAcks, KafkaCompression, KafkaConnectionConfig, KafkaProducerConfig,
        KafkaSaslMechanism, KafkaSecurityProtocol,
    },
    handle::{KafkaHandle, OutboundPublish},
};
use async_trait::async_trait;
use futures_util::StreamExt;
use ng_gateway_sdk::{
    northward::{
        codec::DecodeError,
        downlink::{decode_event, AckPolicy, DownlinkMessageMeta, DownlinkRouteTable, KeyValue},
    },
    supervision::{RunOutcome, Session, SessionContext},
    NorthwardError, NorthwardEvent, RetryController, RetryDecision, RetryPolicy,
};
use rdkafka::{
    consumer::{CommitMode, Consumer, StreamConsumer},
    error::KafkaError,
    message::{BorrowedMessage, Header, Headers, OwnedHeaders},
    producer::{FutureProducer, FutureRecord},
    ClientConfig, Message,
};
use std::{collections::HashMap, sync::Arc};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Kafka supervised session for a single attempt.
pub struct KafkaSession {
    handle: Arc<KafkaHandle>,
    producer: FutureProducer,
    outbound_rx: mpsc::Receiver<OutboundPublish>,
    conn: KafkaConnectionConfig,
    downlink_routes: Option<Arc<DownlinkRouteTable>>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    retry_policy: RetryPolicy,
    app_id: i32,
}

/// Construction arguments for [`KafkaSession`].
///
/// This is intentionally a single struct to keep call sites readable while
/// satisfying `clippy::too_many_arguments` when building attempt-scoped sessions.
pub struct KafkaSessionArgs {
    /// Shared handle published to the SDK supervisor.
    pub handle: Arc<KafkaHandle>,
    /// Connected Kafka producer created in `connect()`.
    pub producer: FutureProducer,
    /// Attempt-scoped outbound publish queue.
    pub outbound_rx: mpsc::Receiver<OutboundPublish>,
    /// Connection config snapshot for this attempt.
    pub conn: KafkaConnectionConfig,
    /// Optional pre-built downlink routing table.
    pub downlink_routes: Option<Arc<DownlinkRouteTable>>,
    /// Event bus sender for decoded downlink events.
    pub events_tx: mpsc::Sender<NorthwardEvent>,
    /// Retry policy for downlink consumer self-healing loop.
    pub retry_policy: RetryPolicy,
    /// Owning application id (used for logging / identifiers).
    pub app_id: i32,
}

impl KafkaSession {
    /// Create a new attempt-scoped [`KafkaSession`].
    pub fn new(args: KafkaSessionArgs) -> Self {
        Self {
            handle: args.handle,
            producer: args.producer,
            outbound_rx: args.outbound_rx,
            conn: args.conn,
            downlink_routes: args.downlink_routes,
            events_tx: args.events_tx,
            retry_policy: args.retry_policy,
            app_id: args.app_id,
        }
    }
}

#[async_trait]
impl Session for KafkaSession {
    type Handle = KafkaHandle;
    type Error = NorthwardError;

    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, _ctx: &SessionContext) -> Result<(), Self::Error> {
        // Producer has already been created successfully in `connect()`.
        Ok(())
    }

    async fn run(mut self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        let cancel = ctx.cancel.clone();
        let reconnect = ctx.reconnect.clone();

        // Publisher task owns Kafka I/O.
        let producer = self.producer;
        let mut outbound_rx = self.outbound_rx;
        let app_id = self.app_id;
        let publisher_cancel = cancel.child_token();
        let publisher_reconnect = reconnect.clone();
        let publisher_task = tokio::spawn(async move {
            spawn_publisher_loop(
                app_id,
                producer,
                &mut outbound_rx,
                publisher_reconnect,
                publisher_cancel,
            )
            .await;
        });

        // Optional downlink consumer supervisor loop (best-effort, self-healing).
        let consumer_task = if let Some(routes) = self.downlink_routes.take() {
            if routes.topics.is_empty() {
                None
            } else {
                let conn = self.conn.clone();
                let events_tx = self.events_tx.clone();
                let retry_policy = self.retry_policy;
                let consumer_cancel = cancel.child_token();
                Some(tokio::spawn(async move {
                    run_consumer_supervisor(
                        app_id,
                        conn,
                        routes,
                        events_tx,
                        retry_policy,
                        consumer_cancel,
                    )
                    .await;
                }))
            }
        } else {
            None
        };

        // Publisher and consumer are treated as peers:
        // - If either task exits unexpectedly, the session is considered unhealthy.
        // - We request an immediate reconnect to rebuild both sides.
        let outcome = if let Some(mut consumer_task) = consumer_task {
            tokio::select! {
                _ = ctx.cancel.cancelled() => RunOutcome::Disconnected,
                res = publisher_task => {
                    let _ = res; // JoinError is logged/handled by supervisor error accounting.
                    RunOutcome::ReconnectRequested(Arc::<str>::from("kafka publisher task exited"))
                }
                res = &mut consumer_task => {
                    let _ = res;
                    RunOutcome::ReconnectRequested(Arc::<str>::from("kafka consumer task exited"))
                }
            }
        } else {
            tokio::select! {
                _ = ctx.cancel.cancelled() => RunOutcome::Disconnected,
                res = publisher_task => {
                    let _ = res;
                    RunOutcome::ReconnectRequested(Arc::<str>::from("kafka publisher task exited"))
                }
            }
        };

        // Best-effort join on cancellation path.
        if matches!(outcome, RunOutcome::Disconnected) {
            // Allow tasks to observe cancellation and exit.
            let _ = cancel.cancel();
        } else {
            // Trigger reconnect quickly from inside session (best-effort).
            let _ = reconnect.try_request_reconnect("peer task exited");
        }

        Ok(outcome)
    }
}

async fn spawn_publisher_loop(
    app_id: i32,
    producer: FutureProducer,
    rx: &mut mpsc::Receiver<OutboundPublish>,
    reconnect: ng_gateway_sdk::supervision::ReconnectHandle,
    cancel: CancellationToken,
) {
    /// Publisher loop owns the send-path side effects (Kafka I/O + delivery receipts).
    ///
    /// Design goals:
    /// - Keep `process_data()` CPU-only and non-blocking for high throughput.
    /// - Centralize error handling / reconnect triggers.
    /// - Bound in-flight delivery awaits to avoid unbounded task growth.
    const MAX_INFLIGHT_DELIVERIES: usize = 1024;

    let mut inflight = JoinSet::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            maybe = rx.recv() => {
                let Some(p) = maybe else { break; };

                while inflight.len() >= MAX_INFLIGHT_DELIVERIES {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = inflight.join_next() => {}
                    }
                }

                let headers = build_headers(&p.msg.headers);
                let mut record = FutureRecord::to(p.topic.as_str()).payload(&p.msg.payload);
                if let Some(key) = p.msg.key.as_ref() {
                    record = record.key(key.as_str());
                }
                if let Some(ts_ms) = p.msg.ts_ms {
                    record = record.timestamp(ts_ms);
                }
                record = record.headers(headers);

                match producer.send_result(record) {
                    Ok(delivery_f) => {
                        let reconnect = reconnect.clone();
                        let topic = p.topic;
                        inflight.spawn(async move {
                            match delivery_f.await {
                                Ok(Ok(_delivery)) => {
                                    debug!(app_id, topic=%topic, "kafka delivery ok");
                                }
                                Ok(Err((e, _msg))) => {
                                    warn!(app_id, topic=%topic, error=%e, "kafka delivery failed");
                                    let _ = reconnect.try_request_reconnect(e.to_string());
                                }
                                Err(e) => {
                                    warn!(app_id, topic=%topic, error=%e, "kafka delivery future cancelled");
                                    let _ = reconnect.try_request_reconnect(format!("delivery future cancelled: {e}"));
                                }
                            }
                        });
                    }
                    Err((e, _owned_msg)) => {
                        warn!(app_id, topic=%p.topic, error=%e, "kafka send_result failed");
                        let _ = reconnect.try_request_reconnect(e.to_string());
                    }
                }
            }
        }
    }

    while inflight.join_next().await.is_some() {}
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

async fn run_consumer_supervisor(
    app_id: i32,
    conn: KafkaConnectionConfig,
    routes: Arc<DownlinkRouteTable>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    retry_policy: RetryPolicy,
    cancel: CancellationToken,
) {
    let mut retry = RetryController::new(&retry_policy);
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match run_downlink_consumer_session(app_id, &conn, routes.as_ref(), &events_tx, &cancel)
            .await
        {
            Ok(()) => {
                retry.reset();
                break;
            }
            Err(e) => {
                warn!(app_id, error=%e, "Kafka downlink session failed");
                match retry.on_failure() {
                    RetryDecision::RetryAfter(delay) => {
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep(delay) => {}
                        }
                    }
                    RetryDecision::Exhausted => break,
                }
            }
        }
    }
}

async fn run_downlink_consumer_session(
    app_id: i32,
    conn: &KafkaConnectionConfig,
    routes: &DownlinkRouteTable,
    events_tx: &mpsc::Sender<NorthwardEvent>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    debug!(app_id, topics = ?routes.topics, "Building Kafka downlink consumer");

    let group_id = format!("ng-gateway-plugin-{}", app_id);

    let mut cfg = build_client_config_base(app_id, conn);
    cfg.set("group.id", group_id.as_str());
    cfg.set("enable.auto.commit", "false");
    cfg.set("auto.offset.reset", "latest");

    let consumer: StreamConsumer = cfg
        .create()
        .map_err(|e| format!("downlink consumer create failed: {e}"))?;

    let topics: Vec<&str> = routes.topics.iter().map(|s| s.as_str()).collect();
    consumer
        .subscribe(&topics)
        .map_err(|e| format!("downlink consumer subscribe failed: {e}"))?;

    let mut stream = consumer.stream();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            maybe = stream.next() => {
                let Some(item) = maybe else { return Err("downlink consumer stream ended".to_string()); };
                let msg = item.map_err(|e| format!("downlink consumer recv error: {e}"))?;
                handle_downlink_message(&consumer, routes, events_tx, &msg).await?;
            }
        }
    }
}

async fn handle_downlink_message(
    consumer: &StreamConsumer,
    routes: &DownlinkRouteTable,
    events_tx: &mpsc::Sender<NorthwardEvent>,
    msg: &BorrowedMessage<'_>,
) -> Result<(), String> {
    let topic = msg.topic();
    let Some(route_list) = routes.by_topic.get(topic) else {
        // Topic not configured; commit to avoid poison messages.
        let _ = consumer.commit_message(msg, CommitMode::Async);
        return Ok(());
    };

    let Some(policy_route) = route_list.first() else {
        let _ = consumer.commit_message(msg, CommitMode::Async);
        return Ok(());
    };
    let policy = &policy_route.mapping;

    let payload = msg.payload().unwrap_or(&[]);
    let key = msg.key().and_then(|k| std::str::from_utf8(k).ok());

    // Convert Kafka headers to SDK KeyValue pairs (best-effort UTF-8 only).
    let mut owned_kvs: Vec<(String, String)> = Vec::new();
    if let Some(hdrs) = msg.headers() {
        for i in 0..hdrs.count() {
            let h = hdrs.get(i);
            let key_s = h.key.to_string();
            let value_s = match h.value {
                Some(v) => match std::str::from_utf8(v) {
                    Ok(s) => s.to_string(),
                    Err(_) => continue,
                },
                None => continue,
            };
            owned_kvs.push((key_s, value_s));
        }
    }

    let kvs: Vec<KeyValue<'_>> = owned_kvs
        .iter()
        .map(|(k, v)| KeyValue {
            key: k.as_str(),
            value: v.as_str(),
        })
        .collect();

    let meta = DownlinkMessageMeta {
        key,
        properties: if kvs.is_empty() {
            None
        } else {
            Some(kvs.as_slice())
        },
    };

    let mut forwarded = false;
    let mut last_error: Option<DecodeError> = None;

    for route in route_list.iter() {
        match decode_event(route, &meta, payload) {
            Ok(Some(ev)) => {
                if events_tx.send(ev).await.is_ok() {
                    forwarded = true;
                    break;
                } else {
                    last_error = Some(DecodeError::Payload("events channel closed".to_string()));
                    break;
                }
            }
            Ok(None) => continue, // not matched
            Err(e) => {
                last_error = Some(e);
                continue;
            }
        }
    }

    // ok semantics for AckPolicy::OnSuccess:
    // - forwarded => true
    // - ignored (no match & no errors) => true
    // - failed (decode/filter/forward errors) => false
    let ok = forwarded || last_error.is_none();

    let mut should_commit = false;
    match policy.ack_policy {
        AckPolicy::Never => {}
        AckPolicy::Always => {
            should_commit = true;
        }
        AckPolicy::OnSuccess => {
            if ok {
                should_commit = true;
            } else {
                match policy.failure_policy {
                    crate::config::FailurePolicy::Drop => {
                        should_commit = true;
                    }
                    crate::config::FailurePolicy::Error => {
                        should_commit = false;
                    }
                }
            }
        }
    }

    if should_commit {
        consumer
            .commit_message(msg, CommitMode::Async)
            .map_err(|e| format!("commit failed: {e}"))?;
    } else if !ok {
        if let Some(e) = last_error.as_ref() {
            tracing::error!(topic=%topic, error=%e, "Failed to handle downlink message");
        }
    }

    Ok(())
}

/// Create and probe a Kafka producer.
pub(crate) async fn connect_kafka_producer(
    app_id: i32,
    conn: &KafkaConnectionConfig,
    producer_cfg: &KafkaProducerConfig,
) -> Result<FutureProducer, KafkaError> {
    let mut cfg = build_client_config_base(app_id, conn);
    apply_producer_config(&mut cfg, producer_cfg);
    cfg.create()
}

fn build_client_config_base(app_id: i32, conn: &KafkaConnectionConfig) -> ClientConfig {
    let mut cfg = ClientConfig::new();
    cfg.set("bootstrap.servers", conn.bootstrap_servers.as_str());

    let client_id = conn
        .client_id
        .clone()
        .unwrap_or(format!("ng-gateway-app-{}", app_id));
    cfg.set("client.id", client_id.as_str());

    match conn.security.protocol {
        KafkaSecurityProtocol::Plaintext => {
            cfg.set("security.protocol", "PLAINTEXT");
        }
        KafkaSecurityProtocol::Ssl => {
            cfg.set("security.protocol", "SSL");
            if let Some(tls) = conn.security.tls.as_ref() {
                apply_tls_config(&mut cfg, tls);
            }
        }
        KafkaSecurityProtocol::SaslPlaintext => {
            cfg.set("security.protocol", "SASL_PLAINTEXT");
            if let Some(sasl) = conn.security.sasl.as_ref() {
                apply_sasl_config(&mut cfg, sasl);
            }
        }
        KafkaSecurityProtocol::SaslSsl => {
            cfg.set("security.protocol", "SASL_SSL");
            if let Some(tls) = conn.security.tls.as_ref() {
                apply_tls_config(&mut cfg, tls);
            }
            if let Some(sasl) = conn.security.sasl.as_ref() {
                apply_sasl_config(&mut cfg, sasl);
            }
        }
    }

    cfg
}

fn apply_tls_config(cfg: &mut ClientConfig, tls: &crate::config::KafkaTlsConfig) {
    if let Some(v) = tls.ca_location.as_deref() {
        cfg.set("ssl.ca.location", v);
    }
    if let Some(v) = tls.certificate_location.as_deref() {
        cfg.set("ssl.certificate.location", v);
    }
    if let Some(v) = tls.key_location.as_deref() {
        cfg.set("ssl.key.location", v);
    }
    if let Some(v) = tls.key_password.as_deref() {
        cfg.set("ssl.key.password", v);
    }
    if let Some(v) = tls.endpoint_identification_algorithm.as_deref() {
        cfg.set("ssl.endpoint.identification.algorithm", v);
    }
}

fn apply_sasl_config(cfg: &mut ClientConfig, sasl: &crate::config::KafkaSaslConfig) {
    let mechanism = match sasl.mechanism {
        KafkaSaslMechanism::Plain => "PLAIN",
        KafkaSaslMechanism::ScramSha256 => "SCRAM-SHA-256",
        KafkaSaslMechanism::ScramSha512 => "SCRAM-SHA-512",
    };
    cfg.set("sasl.mechanisms", mechanism);
    cfg.set("sasl.username", sasl.username.as_str());
    cfg.set("sasl.password", sasl.password.as_str());
}

fn apply_producer_config(cfg: &mut ClientConfig, producer_cfg: &KafkaProducerConfig) {
    cfg.set(
        "acks",
        match producer_cfg.acks {
            KafkaAcks::None => "0",
            KafkaAcks::One => "1",
            KafkaAcks::All => "all",
        },
    );

    cfg.set(
        "compression.type",
        match producer_cfg.compression {
            KafkaCompression::None => "none",
            KafkaCompression::Gzip => "gzip",
            KafkaCompression::Snappy => "snappy",
            KafkaCompression::Lz4 => "lz4",
            KafkaCompression::Zstd => "zstd",
        },
    );

    cfg.set("linger.ms", producer_cfg.linger_ms.to_string());
    cfg.set(
        "batch.num.messages",
        producer_cfg.batch_num_messages.to_string(),
    );
    cfg.set("batch.size", producer_cfg.batch_size_bytes.to_string());
    cfg.set(
        "message.timeout.ms",
        producer_cfg.message_timeout_ms.to_string(),
    );
    cfg.set(
        "request.timeout.ms",
        producer_cfg.request_timeout_ms.to_string(),
    );
    cfg.set(
        "max.in.flight.requests.per.connection",
        producer_cfg.max_inflight.to_string(),
    );
    cfg.set(
        "enable.idempotence",
        if producer_cfg.enable_idempotence {
            "true"
        } else {
            "false"
        },
    );
}
