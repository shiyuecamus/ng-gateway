//! Kafka connection supervisor with auto-reconnect.
//!
//! This module owns:
//! - Creating Kafka producers/consumers (session init)
//! - Retry/backoff strategy (gateway-level `RetryPolicy`)
//! - Connection state updates (`NorthwardConnectionState`)
//! - Downlink consumer task lifecycle (session-scoped)

use super::config::{
    KafkaAcks, KafkaCompression, KafkaConnectionConfig, KafkaProducerConfig, KafkaSaslMechanism,
    KafkaSecurityProtocol,
};
use arc_swap::ArcSwapOption;
use backoff::backoff::Backoff;
use futures_util::StreamExt;
use ng_gateway_sdk::northward::downlink::{
    decode_event, DownlinkMessageMeta, DownlinkRouteTable, KeyValue,
};
use ng_gateway_sdk::{
    build_exponential_backoff, northward::codec::DecodeError, NorthwardConnectionState,
    NorthwardEvent, RetryPolicy,
};
use rdkafka::{
    consumer::{CommitMode, Consumer, StreamConsumer},
    error::KafkaError,
    message::{BorrowedMessage, Headers},
    producer::FutureProducer,
    ClientConfig, Message,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Shared client entry for lock-free access pattern.
pub(super) struct ClientEntry {
    /// `FutureProducer` wrapped in `ArcSwapOption` for lock-free reads of the handle.
    pub producer: ArcSwapOption<FutureProducer>,
    pub healthy: AtomicBool,
    pub shutdown: AtomicBool,
    pub last_error: std::sync::Mutex<Option<String>>,
}

impl ClientEntry {
    /// Create a new empty client entry.
    #[inline]
    pub fn new_empty() -> Self {
        Self {
            producer: ArcSwapOption::from(None),
            healthy: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            last_error: std::sync::Mutex::new(None),
        }
    }

    /// Update last error message (best-effort, lock-protected).
    ///
    /// Note: takes `&str` to avoid unnecessary `String` clones at call sites.
    #[inline]
    pub fn update_error(&self, error: &str) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error.to_owned());
        }
    }

    /// Clear last error message (best-effort).
    #[inline]
    pub fn clear_error(&self) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = None;
        }
    }
}

pub(super) type SharedClient = Arc<ClientEntry>;

/// Kafka connection supervisor with auto-reconnect (aligned with other supervisors).
pub(super) struct KafkaSupervisor {
    app_id: i32,
    conn: KafkaConnectionConfig,
    producer_cfg: KafkaProducerConfig,
    /// Immutable downlink routes shared across reconnect sessions.
    downlink_routes: Option<Arc<DownlinkRouteTable>>,
    retry_policy: RetryPolicy,
    cancel: CancellationToken,
    state_tx: watch::Sender<NorthwardConnectionState>,
    shared_client: SharedClient,
    /// Best-effort trigger channel from send-path; this supervisor only consumes `reconnect_rx`.
    ///
    /// NOTE: Consumer loop must not rely on this channel to manage its own availability.
    reconnect_tx: mpsc::Sender<String>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    /// Best-effort trigger from send-path to request reconnect.
    reconnect_rx: mpsc::Receiver<String>,
}

pub(super) struct KafkaSupervisorParams {
    pub app_id: i32,
    pub conn: KafkaConnectionConfig,
    pub producer_cfg: KafkaProducerConfig,
    pub downlink_routes: Option<Arc<DownlinkRouteTable>>,
    pub retry_policy: RetryPolicy,
    pub cancel: CancellationToken,
    pub state_tx: watch::Sender<NorthwardConnectionState>,
    pub shared_client: SharedClient,
    pub reconnect_tx: mpsc::Sender<String>,
    pub events_tx: mpsc::Sender<NorthwardEvent>,
    pub reconnect_rx: mpsc::Receiver<String>,
}

impl KafkaSupervisor {
    pub fn new(params: KafkaSupervisorParams) -> Self {
        Self {
            app_id: params.app_id,
            conn: params.conn,
            producer_cfg: params.producer_cfg,
            downlink_routes: params.downlink_routes,
            retry_policy: params.retry_policy,
            cancel: params.cancel,
            state_tx: params.state_tx,
            shared_client: params.shared_client,
            reconnect_tx: params.reconnect_tx,
            events_tx: params.events_tx,
            reconnect_rx: params.reconnect_rx,
        }
    }

    pub async fn run(self) {
        let KafkaSupervisor {
            app_id,
            conn,
            producer_cfg,
            downlink_routes,
            retry_policy,
            cancel,
            state_tx,
            shared_client,
            reconnect_tx: _reconnect_tx,
            events_tx,
            reconnect_rx,
        } = self;

        // Best-practice: producer HA and consumer listen-loop are independent concerns.
        //
        // - Producer loop owns uplink availability and connection state (`state_tx`).
        // - Consumer loop is session-independent and self-heals with its own backoff.
        //
        // Rationale:
        // - Kafka producer and consumer are separate clients/protocol flows.
        // - Downlink should keep attempting even if uplink is temporarily unavailable.
        // - Consumer failures must not trigger producer reconnect by default.
        spawn_producer_supervisor_task(ProducerSupervisorParams {
            app_id,
            conn: conn.clone(),
            producer_cfg,
            retry_policy,
            cancel: cancel.child_token(),
            state_tx,
            shared_client: Arc::clone(&shared_client),
            reconnect_rx,
        });

        if let Some(routes) = downlink_routes.as_ref() {
            if !routes.topics.is_empty() {
                spawn_consumer_supervisor_task(
                    app_id,
                    conn,
                    Arc::clone(routes),
                    events_tx,
                    retry_policy,
                    cancel.child_token(),
                );
            }
        }
    }
}

/// Parameters for the producer supervisor task.
///
/// This struct exists to keep the supervisor spawn API stable and avoid
/// `clippy::too_many_arguments`.
struct ProducerSupervisorParams {
    app_id: i32,
    conn: KafkaConnectionConfig,
    producer_cfg: KafkaProducerConfig,
    retry_policy: RetryPolicy,
    cancel: CancellationToken,
    state_tx: watch::Sender<NorthwardConnectionState>,
    shared_client: SharedClient,
    reconnect_rx: mpsc::Receiver<String>,
}

fn spawn_producer_supervisor_task(params: ProducerSupervisorParams) {
    let ProducerSupervisorParams {
        app_id,
        conn,
        producer_cfg,
        retry_policy,
        cancel,
        state_tx,
        shared_client,
        mut reconnect_rx,
    } = params;
    tokio::spawn(async move {
        let mut bo = build_exponential_backoff(&retry_policy);
        let mut attempt: u32 = 0;

        let should_retry = |current_attempt: u32| -> bool {
            match retry_policy.max_attempts {
                None | Some(0) => true,
                Some(max) => current_attempt < max,
            }
        };

        loop {
            if cancel.is_cancelled() {
                shared_client.producer.store(None);
                shared_client.healthy.store(false, Ordering::Release);
                let _ = state_tx.send(NorthwardConnectionState::Disconnected);
                break;
            }

            if !should_retry(attempt) {
                let _ = state_tx.send(NorthwardConnectionState::Failed(format!(
                    "Max retry attempts ({:?}) exhausted",
                    retry_policy.max_attempts
                )));
                break;
            }

            attempt += 1;
            let _ = state_tx.send(NorthwardConnectionState::Connecting);
            info!(attempt, "Kafka producer supervisor attempting connection");

            match connect_kafka_producer(app_id, &conn, &producer_cfg).await {
                Ok(producer) => {
                    shared_client.producer.store(Some(Arc::new(producer)));
                    shared_client.healthy.store(true, Ordering::Release);
                    shared_client.clear_error();
                    let _ = state_tx.send(NorthwardConnectionState::Connected);
                    bo.reset();
                    attempt = 0;

                    // Connected loop: wait for reconnect request or shutdown.
                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                shared_client.producer.store(None);
                                shared_client.healthy.store(false, Ordering::Release);
                                let _ = state_tx.send(NorthwardConnectionState::Disconnected);
                                return;
                            }
                            msg = reconnect_rx.recv() => {
                                if let Some(msg) = msg {
                                    warn!(reason=%msg, "Kafka producer supervisor received reconnect request");
                                    shared_client.update_error(&msg);
                                    shared_client.producer.store(None);
                                    shared_client.healthy.store(false, Ordering::Release);
                                    let _ = state_tx.send(NorthwardConnectionState::Reconnecting);
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    warn!(attempt, error=%error_msg, "Failed to create Kafka producer");
                    shared_client.update_error(&error_msg);
                    shared_client.producer.store(None);
                    shared_client.healthy.store(false, Ordering::Release);
                    let _ = state_tx.send(NorthwardConnectionState::Failed(error_msg));

                    if !should_retry(attempt) {
                        break;
                    }

                    match bo.next_backoff() {
                        Some(delay) => {
                            tokio::select! {
                                _ = cancel.cancelled() => break,
                                _ = tokio::time::sleep(delay) => {}
                            }
                        }
                        None => {
                            let _ = state_tx.send(NorthwardConnectionState::Failed(
                                "Backoff time exhausted".to_string(),
                            ));
                            break;
                        }
                    }
                }
            }
        }

        info!("Kafka producer supervisor loop terminated");
    });
}

fn spawn_consumer_supervisor_task(
    app_id: i32,
    conn: KafkaConnectionConfig,
    routes: Arc<DownlinkRouteTable>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    retry_policy: RetryPolicy,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut bo = build_exponential_backoff(&retry_policy);
        let mut attempt: u32 = 0;

        let should_retry = |current_attempt: u32| -> bool {
            match retry_policy.max_attempts {
                None | Some(0) => true,
                Some(max) => current_attempt < max,
            }
        };

        loop {
            if cancel.is_cancelled() {
                break;
            }

            if !should_retry(attempt) {
                warn!(
                    attempt,
                    "Kafka consumer supervisor exhausted max retry attempts ({:?}); stopping downlink",
                    retry_policy.max_attempts
                );
                break;
            }

            attempt += 1;
            info!(
                attempt,
                "Kafka consumer supervisor starting downlink session"
            );

            match run_downlink_consumer_session(app_id, &conn, &routes, &events_tx, &cancel).await {
                Ok(()) => {
                    // Normal exit (usually cancellation). Stop the supervisor.
                    bo.reset();
                    break;
                }
                Err(e) => {
                    warn!(attempt, error=%e, "Kafka downlink session failed");

                    match bo.next_backoff() {
                        Some(delay) => {
                            tokio::select! {
                                _ = cancel.cancelled() => break,
                                _ = tokio::time::sleep(delay) => {}
                            }
                        }
                        None => {
                            warn!("Kafka consumer supervisor backoff exhausted; stopping downlink");
                            break;
                        }
                    }
                }
            }
        }

        info!("Kafka consumer supervisor loop terminated");
    });
}

/// Create and probe a Kafka producer.
async fn connect_kafka_producer(
    app_id: i32,
    conn: &KafkaConnectionConfig,
    producer_cfg: &KafkaProducerConfig,
) -> Result<FutureProducer, KafkaError> {
    let mut cfg = build_client_config_base(app_id, conn);
    apply_producer_config(&mut cfg, producer_cfg);

    let producer: FutureProducer = cfg.create()?;

    Ok(producer)
}

fn build_client_config_base(app_id: i32, conn: &KafkaConnectionConfig) -> ClientConfig {
    let mut cfg = ClientConfig::new();
    cfg.set("bootstrap.servers", conn.bootstrap_servers.as_str());

    let client_id = conn
        .client_id
        .clone()
        .unwrap_or_else(|| format!("ng-gateway-app-{}", app_id));
    cfg.set("client.id", client_id.as_str());

    // Security protocol + optional TLS/SASL
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

fn apply_tls_config(cfg: &mut ClientConfig, tls: &super::config::KafkaTlsConfig) {
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

fn apply_sasl_config(cfg: &mut ClientConfig, sasl: &super::config::KafkaSaslConfig) {
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

async fn run_downlink_consumer_session(
    app_id: i32,
    conn: &KafkaConnectionConfig,
    routes: &DownlinkRouteTable,
    events_tx: &mpsc::Sender<NorthwardEvent>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    tracing::debug!(topics = ?routes.topics, "Building Kafka downlink consumer");

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
        super::config::AckPolicy::Never => {}
        super::config::AckPolicy::Always => {
            should_commit = true;
        }
        super::config::AckPolicy::OnSuccess => {
            if ok {
                should_commit = true;
            } else {
                match policy.failure_policy {
                    super::config::FailurePolicy::Drop => {
                        should_commit = true;
                    }
                    super::config::FailurePolicy::Error => {
                        should_commit = false;
                    }
                }
            }
        }
    }

    if should_commit {
        // Async commit is fine here; consumer is session-scoped and reconnect will rebalance anyway.
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
