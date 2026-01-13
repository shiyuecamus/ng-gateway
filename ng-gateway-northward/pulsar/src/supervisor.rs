use super::config::{
    AckPolicy, FailurePolicy, PulsarAuthConfig, PulsarCompression, PulsarConnectionConfig,
    PulsarProducerConfig,
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
use pulsar::{
    compression::Compression,
    message::proto::command_subscribe::SubType,
    producer::{MultiTopicProducer, ProducerOptions},
    Authentication, ConnectionRetryOptions, Pulsar, TokioExecutor,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, watch, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Shared client entry for lock-free access pattern.
pub(super) struct ClientEntry {
    /// Multi-topic producer wrapped in `Arc<Mutex<...>>`; `ArcSwapOption` enables lock-free reads of the handle.
    pub producer: ArcSwapOption<Mutex<MultiTopicProducer<TokioExecutor>>>,
    pub healthy: AtomicBool,
    pub shutdown: AtomicBool,
    pub last_error: std::sync::Mutex<Option<String>>,
}

impl ClientEntry {
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

    #[inline]
    pub fn clear_error(&self) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = None;
        }
    }
}

pub(super) type SharedClient = Arc<ClientEntry>;

/// Pulsar connection supervisor with auto-reconnect (aligned with other supervisors).
pub(super) struct PulsarSupervisor {
    app_id: i32,
    conn: PulsarConnectionConfig,
    producer_cfg: PulsarProducerConfig,
    /// Immutable downlink routes shared across reconnect sessions.
    ///
    /// We intentionally use `Arc<[T]>` to:
    /// - Avoid deep-cloning route tables (topics/mappings/matchers) on every reconnect.
    /// - Provide cheap clones for session-scoped consumer tasks.
    downlink_routes: Option<Arc<DownlinkRouteTable>>,
    retry_policy: RetryPolicy,
    cancel: CancellationToken,
    state_tx: watch::Sender<NorthwardConnectionState>,
    shared_client: SharedClient,
    reconnect_tx: mpsc::Sender<String>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    /// Best-effort trigger from send-path to request reconnect.
    reconnect_rx: mpsc::Receiver<String>,
}

pub(super) struct PulsarSupervisorParams {
    pub app_id: i32,
    pub conn: PulsarConnectionConfig,
    pub producer_cfg: PulsarProducerConfig,
    pub downlink_routes: Option<Arc<DownlinkRouteTable>>,
    pub retry_policy: RetryPolicy,
    pub cancel: CancellationToken,
    pub state_tx: watch::Sender<NorthwardConnectionState>,
    pub shared_client: SharedClient,
    pub reconnect_tx: mpsc::Sender<String>,
    pub events_tx: mpsc::Sender<NorthwardEvent>,
    pub reconnect_rx: mpsc::Receiver<String>,
}

impl PulsarSupervisor {
    pub fn new(params: PulsarSupervisorParams) -> Self {
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
        let PulsarSupervisor {
            app_id,
            conn,
            producer_cfg,
            downlink_routes,
            retry_policy,
            cancel,
            state_tx,
            shared_client,
            reconnect_tx,
            events_tx,
            mut reconnect_rx,
        } = self;

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
                    // Normal shutdown path should not be reported as "Failed".
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
                info!(attempt, "Pulsar supervisor attempting connection");

                match connect_pulsar_client(&conn).await {
                    Ok(pulsar_client) => {
                        // Session = one connected client + initialized producer + (optional) consumer tasks.
                        //
                        // Design:
                        // - `run()` loop owns HA (retry/backoff/reconnect triggers).
                        // - A session has its own cancellation token; reconnect cancels the session first, then rebuilds.
                        // - Producer initialization is part of session init (uplink depends on producer availability).
                        let session_cancel = cancel.child_token();

                        // Init producer (fast, non-streaming). If this fails, treat as connect failure and retry.
                        let producer = build_multi_topic_producer(&pulsar_client, &producer_cfg);
                        shared_client
                            .producer
                            .store(Some(Arc::new(Mutex::new(producer))));
                        shared_client.healthy.store(true, Ordering::Release);
                        shared_client.clear_error();
                        let _ = state_tx.send(NorthwardConnectionState::Connected);
                        bo.reset();
                        attempt = 0;

                        // Start downlink consumer task (streaming) for this session.
                        //
                        // Downlink subscription is controlled by `downlink.enabled` at config-level.
                        // Here we only require a usable subscription name and at least one enabled route.
                        if let Some(routes) = downlink_routes.as_ref() {
                            if routes.topics.is_empty() {
                                // No enabled routes; skip starting the streaming consumer task.
                            } else {
                                let routes_for_task = Arc::clone(routes);
                                let events_tx = events_tx.clone();
                                let reconnect_tx = reconnect_tx.clone();
                                let session_cancel_child = session_cancel.child_token();
                                let client = pulsar_client.clone();
                                tokio::spawn(async move {
                                    run_downlink_consumer_task(
                                        app_id,
                                        client,
                                        routes_for_task,
                                        events_tx,
                                        reconnect_tx,
                                        session_cancel_child,
                                    )
                                    .await;
                                });
                            }
                        }

                        // Session loop: reconnect/cancel, or downlink task exits unexpectedly.
                        loop {
                            tokio::select! {
                                _ = cancel.cancelled() => {
                                    session_cancel.cancel();
                                    shared_client.producer.store(None);
                                    shared_client.healthy.store(false, Ordering::Release);
                                    let _ = state_tx.send(NorthwardConnectionState::Disconnected);
                                    return;
                                }
                                msg = reconnect_rx.recv() => {
                                    if let Some(msg) = msg {
                                        warn!(reason=%msg, "Pulsar supervisor received reconnect request");
                                        shared_client.update_error(&msg);
                                        session_cancel.cancel();
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
                        warn!(attempt, error=%error_msg, "Failed to create Pulsar client");
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

            info!("Pulsar supervisor loop terminated");
        });
    }
}

/// Build a Pulsar client (network connection) with configured auth knobs.
///
/// This function should stay focused: it only creates the client.
async fn connect_pulsar_client(
    conn: &PulsarConnectionConfig,
) -> Result<Pulsar<TokioExecutor>, pulsar::Error> {
    let mut builder = Pulsar::builder(conn.service_url.as_str(), TokioExecutor);

    // Auth
    match &conn.auth {
        PulsarAuthConfig::None => {}
        PulsarAuthConfig::Token { token } => {
            builder = builder.with_auth(Authentication {
                name: "token".to_string(),
                data: token.clone().into_bytes(),
            });
        }
    }

    // Connection retry options: keep the crate default; gateway owns higher-level retry policy.
    builder = builder.with_connection_retry_options(ConnectionRetryOptions::default());

    let pulsar: Pulsar<_> = builder.build().await?;
    Ok(pulsar)
}

/// Build a multi-topic producer for a connected client.
///
/// Notes:
/// - MultiTopicProducer is a lightweight factory; per-topic producers are created lazily.
/// - This is intentionally sync to keep session init simple and predictable.
fn build_multi_topic_producer(
    pulsar: &Pulsar<TokioExecutor>,
    producer_cfg: &PulsarProducerConfig,
) -> MultiTopicProducer<TokioExecutor> {
    let opts = build_producer_options(producer_cfg);
    pulsar.producer().with_options(opts).build_multi_topic()
}

fn build_producer_options(cfg: &PulsarProducerConfig) -> ProducerOptions {
    let mut opts = ProducerOptions::default();
    if cfg.batching_enabled {
        opts.batch_size = cfg.batching_max_messages;
        opts.batch_byte_size = cfg.batching_max_bytes.map(|v| v as usize);
        opts.batch_timeout = cfg
            .batching_max_publish_delay_ms
            .map(|ms| std::time::Duration::from_millis(ms as u64));
    }

    opts.compression = Some(match cfg.compression {
        PulsarCompression::None => Compression::None,
        PulsarCompression::Lz4 => Compression::Lz4(Default::default()),
        PulsarCompression::Zlib => Compression::Zlib(Default::default()),
        PulsarCompression::Zstd => Compression::Zstd(Default::default()),
        PulsarCompression::Snappy => Compression::Snappy(Default::default()),
    });
    opts
}

/// Run a downlink consumer loop for the current session.
///
/// Notes:
/// - This task must be session-scoped. It exits when `cancel` is cancelled.
/// - Any unrecoverable error should request a reconnect via `reconnect_tx` and then exit.
async fn run_downlink_consumer_task(
    app_id: i32,
    pulsar: Pulsar<TokioExecutor>,
    routes: Arc<DownlinkRouteTable>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    reconnect_tx: mpsc::Sender<String>,
    cancel: CancellationToken,
) {
    tracing::debug!(topics = ?routes.topics, "Building downlink consumer");

    // Subscription name should be stable per app_id to maintain state on broker side.
    let subscription_name = format!("ng-gateway-plugin-{}", app_id);
    let mut builder = pulsar
        .consumer()
        .with_subscription(subscription_name)
        .with_subscription_type(SubType::Shared);

    if !routes.topics.is_empty() {
        builder = builder.with_topics(routes.topics.as_ref());
    }

    let mut consumer = match builder.build::<Vec<u8>>().await {
        Ok(c) => c,
        Err(e) => {
            let _ = reconnect_tx
                .send(format!("downlink consumer build failed: {e}"))
                .await;
            return;
        }
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            maybe = consumer.next() => {
                let Some(item) = maybe else { break; };
                let msg = match item {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = reconnect_tx.send(format!("downlink consumer recv error: {e}")).await;
                        break;
                    }
                };

                let Some(route_list) = routes.by_topic.get(msg.topic.as_str()) else {
                    // Topic not configured; treat as handled and ack to avoid poison messages.
                    let _ = consumer.ack(&msg).await;
                    continue;
                };

                let Some(policy_route) = route_list.first() else {
                    let _ = consumer.ack(&msg).await;
                    continue;
                };

                // NOTE: `build_route_table()` enforces that all routes on the same topic share
                // the same ack/failure policy, so using the first route is well-defined.
                let policy = &policy_route.mapping;

                // Best-effort extract metadata for filters. If the pulsar message type does not
                // expose these fields, we keep them as None (those filter modes won't match).
                let md = msg.metadata();

                let mut forwarded = false;
                let mut last_error = None;

                // Convert Pulsar properties to SDK KeyValue pairs
                let properties: Vec<KeyValue> = md.properties.iter()
                    .map(|kv| KeyValue {
                        key: kv.key.as_str(),
                        value: kv.value.as_str(),
                    })
                    .collect();

                let meta = DownlinkMessageMeta {
                    key: md.partition_key.as_deref(),
                    properties: Some(&properties),
                };

                for route in route_list.iter() {
                    match decode_event(route, &meta, &msg.payload.data) {
                        Ok(Some(ev)) => {
                            if events_tx.send(ev).await.is_ok() {
                                forwarded = true;
                                break;
                            } else {
                                last_error = Some(DecodeError::Payload(
                                    "events channel closed".to_string(),
                                ));
                                break;
                            }
                        }
                        Ok(None) => {
                            // Not matched; continue to try next route (normal in mixed-topic mode).
                            continue;
                        }
                        Err(e) => {
                            last_error = Some(e);
                            continue;
                        }
                    }
                }

                // `ok` semantics for AckPolicy::OnSuccess:
                // - forwarded => true
                // - ignored (no match & no errors) => true
                // - failed (decode/filter/forward errors) => false
                let ok = forwarded || last_error.is_none();
                if !ok {
                    if let Some(e) = last_error.as_ref() {
                        tracing::error!(topic=%msg.topic, error=%e, "Failed to handle downlink message");
                    }
                }

                match policy.ack_policy {
                    AckPolicy::Never => {}
                    AckPolicy::Always => {
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = consumer.ack(&msg) => {}
                        }
                    }
                    AckPolicy::OnSuccess => {
                        if ok {
                            tokio::select! {
                                _ = cancel.cancelled() => break,
                                _ = consumer.ack(&msg) => {}
                            }
                        } else {
                            match policy.failure_policy {
                                FailurePolicy::Drop => {
                                    tokio::select! {
                                        _ = cancel.cancelled() => break,
                                        _ = consumer.ack(&msg) => {}
                                    }
                                }
                                FailurePolicy::Error => {
                                    tokio::select! {
                                        _ = cancel.cancelled() => break,
                                        _ = consumer.nack(&msg) => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
