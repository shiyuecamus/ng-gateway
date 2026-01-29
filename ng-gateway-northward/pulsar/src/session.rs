//! Pulsar supervised session implementation.
//!
//! This module contains the per-attempt session lifecycle driven by the SDK supervisor:
//! - `init()`: defines "Ready" (client+producer are created in `connect()`).
//! - `run()`: drives publisher (uplink) and optional downlink consumer loop.

use super::{
    config::{
        AckPolicy, FailurePolicy, PulsarAuthConfig, PulsarCompression, PulsarConnectionConfig,
        PulsarProducerConfig,
    },
    handle::{OutboundPublish, PulsarHandle},
};
use async_trait::async_trait;
use futures_util::StreamExt;
use ng_gateway_sdk::{
    northward::{
        codec::DecodeError,
        downlink::{decode_event, DownlinkMessageMeta, DownlinkRouteTable, KeyValue},
    },
    supervision::{RunOutcome, Session, SessionContext},
    NorthwardError, NorthwardEvent, RetryController, RetryDecision, RetryPolicy,
};
use pulsar::{
    compression::Compression,
    message::proto::command_subscribe::SubType,
    producer::{MultiTopicProducer, ProducerOptions},
    Authentication, ConnectionRetryOptions, Pulsar, TokioExecutor,
};
use std::sync::Arc;
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Pulsar supervised session for a single attempt.
pub struct PulsarSession {
    handle: Arc<PulsarHandle>,
    pulsar: Pulsar<TokioExecutor>,
    producer: MultiTopicProducer<TokioExecutor>,
    outbound_rx: mpsc::Receiver<OutboundPublish>,
    downlink_routes: Option<Arc<DownlinkRouteTable>>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    retry_policy: RetryPolicy,
    app_id: i32,
}

/// Construction arguments for [`PulsarSession`].
///
/// This is intentionally a single struct to keep call sites readable while
/// satisfying `clippy::too_many_arguments` when building attempt-scoped sessions.
pub struct PulsarSessionArgs {
    /// Shared handle published to the SDK supervisor.
    pub handle: Arc<PulsarHandle>,
    /// Connected client created in `connect()`.
    pub pulsar: Pulsar<TokioExecutor>,
    /// Multi-topic producer created in `connect()`.
    pub producer: MultiTopicProducer<TokioExecutor>,
    /// Attempt-scoped outbound publish queue.
    pub outbound_rx: mpsc::Receiver<OutboundPublish>,
    /// Optional pre-built downlink routing table.
    pub downlink_routes: Option<Arc<DownlinkRouteTable>>,
    /// Event bus sender for decoded downlink events.
    pub events_tx: mpsc::Sender<NorthwardEvent>,
    /// Retry policy for downlink consumer self-healing loop.
    pub retry_policy: RetryPolicy,
    /// Owning application id (used for logging / identifiers).
    pub app_id: i32,
}

impl PulsarSession {
    /// Create a new attempt-scoped [`PulsarSession`].
    pub fn new(args: PulsarSessionArgs) -> Self {
        Self {
            handle: args.handle,
            pulsar: args.pulsar,
            producer: args.producer,
            outbound_rx: args.outbound_rx,
            downlink_routes: args.downlink_routes,
            events_tx: args.events_tx,
            retry_policy: args.retry_policy,
            app_id: args.app_id,
        }
    }
}

#[async_trait]
impl Session for PulsarSession {
    type Handle = PulsarHandle;
    type Error = NorthwardError;

    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, _ctx: &SessionContext) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn run(mut self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        let cancel = ctx.cancel.clone();
        let reconnect = ctx.reconnect.clone();
        let app_id = self.app_id;

        let publisher_cancel = cancel.child_token();
        let mut producer = self.producer;
        let mut outbound_rx = self.outbound_rx;
        let publisher_reconnect = reconnect.clone();
        let publisher_task = tokio::spawn(async move {
            spawn_publisher_loop(
                app_id,
                &mut producer,
                &mut outbound_rx,
                publisher_reconnect,
                publisher_cancel,
            )
            .await;
        });

        let consumer_task = if let Some(routes) = self.downlink_routes.take() {
            if routes.topics.is_empty() {
                None
            } else {
                let pulsar = self.pulsar.clone();
                let events_tx = self.events_tx.clone();
                let retry_policy = self.retry_policy;
                let consumer_cancel = cancel.child_token();
                Some(tokio::spawn(async move {
                    run_consumer_supervisor(
                        app_id,
                        pulsar,
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
                    let _ = res;
                    RunOutcome::ReconnectRequested(Arc::<str>::from("pulsar publisher task exited"))
                }
                res = &mut consumer_task => {
                    let _ = res;
                    RunOutcome::ReconnectRequested(Arc::<str>::from("pulsar consumer task exited"))
                }
            }
        } else {
            tokio::select! {
                _ = ctx.cancel.cancelled() => RunOutcome::Disconnected,
                res = publisher_task => {
                    let _ = res;
                    RunOutcome::ReconnectRequested(Arc::<str>::from("pulsar publisher task exited"))
                }
            }
        };

        if matches!(outcome, RunOutcome::Disconnected) {
            let _ = cancel.cancel();
        } else {
            let _ = reconnect.try_request_reconnect("peer task exited");
        }

        Ok(outcome)
    }
}

async fn spawn_publisher_loop(
    app_id: i32,
    producer: &mut MultiTopicProducer<TokioExecutor>,
    rx: &mut mpsc::Receiver<OutboundPublish>,
    reconnect: ng_gateway_sdk::supervision::ReconnectHandle,
    cancel: CancellationToken,
) {
    /// Bound receipt awaits to avoid unbounded task growth.
    const MAX_INFLIGHT_RECEIPTS: usize = 1024;

    let mut inflight = JoinSet::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            maybe = rx.recv() => {
                let Some(p) = maybe else { break; };

                while inflight.len() >= MAX_INFLIGHT_RECEIPTS {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = inflight.join_next() => {}
                    }
                }

                match producer.send_non_blocking(p.topic.clone(), p.msg).await {
                    Ok(receipt_f) => {
                        let reconnect = reconnect.clone();
                        let topic = p.topic;
                        inflight.spawn(async move {
                            match receipt_f.await {
                                Ok(_receipt) => {
                                    debug!(app_id, topic=%topic, "pulsar send receipt ok");
                                }
                                Err(e) => {
                                    warn!(app_id, topic=%topic, error=%e, "pulsar send receipt failed");
                                    let _ = reconnect.try_request_reconnect(e.to_string());
                                }
                            }
                        });
                    }
                    Err(e) => {
                        warn!(app_id, topic=%p.topic, error=%e, "pulsar send_non_blocking failed");
                        let _ = reconnect.try_request_reconnect(e.to_string());
                    }
                }
            }
        }
    }

    while inflight.join_next().await.is_some() {}
}

async fn run_consumer_supervisor(
    app_id: i32,
    pulsar: Pulsar<TokioExecutor>,
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
        match run_downlink_consumer_session(
            app_id,
            pulsar.clone(),
            Arc::clone(&routes),
            events_tx.clone(),
            cancel.child_token(),
        )
        .await
        {
            Ok(()) => {
                retry.reset();
                break;
            }
            Err(e) => {
                warn!(app_id, error=%e, "Pulsar downlink session failed");
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
    pulsar: Pulsar<TokioExecutor>,
    routes: Arc<DownlinkRouteTable>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    cancel: CancellationToken,
) -> Result<(), String> {
    debug!(app_id, topics = ?routes.topics, "Building downlink consumer");

    let subscription_name = format!("ng-gateway-plugin-{}", app_id);
    let mut builder = pulsar
        .consumer()
        .with_subscription(subscription_name)
        .with_subscription_type(SubType::Shared);

    if !routes.topics.is_empty() {
        builder = builder.with_topics(routes.topics.as_ref());
    }

    let mut consumer = builder
        .build::<Vec<u8>>()
        .await
        .map_err(|e| format!("downlink consumer build failed: {e}"))?;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            maybe = consumer.next() => {
                let Some(item) = maybe else { break; };
                let msg = item.map_err(|e| format!("downlink consumer recv error: {e}"))?;

                let Some(route_list) = routes.by_topic.get(msg.topic.as_str()) else {
                    let _ = consumer.ack(&msg).await;
                    continue;
                };

                let Some(policy_route) = route_list.first() else {
                    let _ = consumer.ack(&msg).await;
                    continue;
                };

                // build_route_table enforces per-topic policy consistency.
                let policy = &policy_route.mapping;

                let md = msg.metadata();
                let properties: Vec<KeyValue> = md.properties.iter()
                    .map(|kv| KeyValue {
                        key: kv.key.as_str(),
                        value: kv.value.as_str(),
                    })
                    .collect();

                let meta = DownlinkMessageMeta {
                    key: md.partition_key.as_deref(),
                    properties: if properties.is_empty() { None } else { Some(&properties) },
                };

                let mut forwarded = false;
                let mut last_error: Option<DecodeError> = None;

                for route in route_list.iter() {
                    match decode_event(route, &meta, &msg.payload.data) {
                        Ok(Some(ev)) => {
                            if events_tx.send(ev).await.is_ok() {
                                forwarded = true;
                                break;
                            } else {
                                last_error = Some(DecodeError::Payload("events channel closed".to_string()));
                                break;
                            }
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            last_error = Some(e);
                            continue;
                        }
                    }
                }

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

    Ok(())
}

/// Build a Pulsar client (network connection) with configured auth knobs.
pub(crate) async fn connect_pulsar_client(
    conn: &PulsarConnectionConfig,
) -> Result<Pulsar<TokioExecutor>, pulsar::Error> {
    let mut builder = Pulsar::builder(conn.service_url.as_str(), TokioExecutor);

    match &conn.auth {
        PulsarAuthConfig::None => {}
        PulsarAuthConfig::Token { token } => {
            builder = builder.with_auth(Authentication {
                name: "token".to_string(),
                data: token.clone().into_bytes(),
            });
        }
    }

    builder = builder.with_connection_retry_options(ConnectionRetryOptions::default());
    builder.build().await
}

/// Build a multi-topic producer for a connected client.
pub(crate) fn build_multi_topic_producer(
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
