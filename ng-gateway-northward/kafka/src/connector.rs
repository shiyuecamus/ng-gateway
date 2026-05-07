//! Kafka supervised connector implementation.
//!
//! This module provides the final-form integration for Kafka:
//! - `KafkaConnector`: implements SDK `Connector` and is constructed via `KafkaConnector::new(ctx)`.
//! - `connect()`: creates a Kafka producer and returns a `KafkaSession`.
//!
//! Notes:
//! - Connection governance (retry/backoff/state publication/handle publish) is handled by SDK
//!   `SupervisorLoop`.
//! - This connector MUST NOT spawn long-lived tasks in `new(ctx)`; all attempt-scoped tasks
//!   belong to `KafkaSession::run`.

use super::{
    config::{DownlinkConfig, KafkaPluginConfig},
    handle::KafkaHandle,
    session::{connect_kafka_producer, KafkaSession, KafkaSessionArgs},
};
use ng_gateway_sdk::{
    northward::{
        downlink::{build_route_table, DownlinkKind, DownlinkRoute, DownlinkRouteTable},
        runtime_api::NorthwardRuntimeApi,
    },
    supervision::{Connector, FailureKind, FailurePhase, Session, SessionContext},
    NorthwardError, NorthwardEvent, NorthwardInitContext, NorthwardResult, RetryPolicy,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

/// Kafka supervised connector (constructed from init context, no I/O).
#[derive(Clone)]
pub struct KafkaConnector {
    config: Arc<KafkaPluginConfig>,
    app_id: i32,
    app_name: String,
    runtime: Arc<dyn NorthwardRuntimeApi>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    retry_policy: RetryPolicy,
    downlink_routes: Option<Arc<DownlinkRouteTable>>,
}

impl KafkaConnector {
    /// Create the connector from init context (no I/O).
    pub fn from_init(ctx: NorthwardInitContext) -> NorthwardResult<Self> {
        let config = ctx
            .config
            .downcast_arc::<KafkaPluginConfig>()
            .map_err(|_| NorthwardError::ConfigurationError {
                message: "Failed to downcast to KafkaPluginConfig".to_string(),
            })?;

        let downlink_routes = build_downlink_routes(&config.downlink)
            .map_err(|e| NorthwardError::ConfigurationError { message: e })?;

        Ok(Self {
            config,
            app_id: ctx.app_id,
            app_name: ctx.app_name,
            runtime: ctx.runtime,
            events_tx: ctx.events_tx,
            retry_policy: ctx.retry_policy,
            downlink_routes,
        })
    }
}

#[async_trait::async_trait]
impl Connector for KafkaConnector {
    type InitContext = NorthwardInitContext;
    type Handle = KafkaHandle;
    type Session = KafkaSession;

    #[inline]
    async fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        Self::from_init(ctx)
    }

    async fn connect(
        &self,
        _ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let producer = connect_kafka_producer(
            self.app_id,
            &self.config.connection,
            &self.config.uplink.producer,
        )
        .await
        .map_err(|e| NorthwardError::RuntimeError {
            reason: format!("kafka producer create failed: {e}"),
        })?;

        // Attempt-scoped bounded queue for send-path side effects.
        // Keep this bounded to apply backpressure and avoid unbounded memory growth.
        const MAX_OUTBOUND_QUEUE_CAPACITY: u32 = 1_000_000;
        let mut outbound_capacity = self.config.uplink.outbound_queue_capacity;
        if outbound_capacity == 0 {
            outbound_capacity = 1;
        }
        if outbound_capacity > MAX_OUTBOUND_QUEUE_CAPACITY {
            warn!(
                app_id = self.app_id,
                configured = outbound_capacity,
                capped = MAX_OUTBOUND_QUEUE_CAPACITY,
                "kafka outbound queue capacity too large, capping"
            );
            outbound_capacity = MAX_OUTBOUND_QUEUE_CAPACITY;
        }
        let (outbound_tx, outbound_rx) = mpsc::channel(outbound_capacity as usize);

        Ok(KafkaSession::new(KafkaSessionArgs {
            handle: Arc::new(KafkaHandle::new(
                Arc::clone(&self.config),
                self.app_id,
                self.app_name.clone(),
                Arc::clone(&self.runtime),
                outbound_tx,
            )),
            producer,
            outbound_rx,
            conn: self.config.connection.clone(),
            downlink_routes: self.downlink_routes.clone(),
            events_tx: self.events_tx.clone(),
            retry_policy: self.retry_policy,
            app_id: self.app_id,
        }))
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
