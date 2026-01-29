//! Pulsar supervised connector implementation.
//!
//! This module provides the final-form integration for Pulsar:
//! - `PulsarConnector`: implements SDK `Connector` and is constructed via `PulsarConnector::new(ctx)`.
//! - `connect()`: creates a Pulsar client + producer and returns a `PulsarSession`.
//!
//! Notes:
//! - Connection governance (retry/backoff/state publication/handle publish) is handled by SDK
//!   `SupervisorLoop`.
//! - This connector MUST NOT spawn long-lived tasks in `new(ctx)`; all attempt-scoped tasks
//!   belong to `PulsarSession::run`.

use super::{
    config::{DownlinkConfig, PulsarPluginConfig},
    handle::PulsarHandle,
    session::{
        build_multi_topic_producer, connect_pulsar_client, PulsarSession, PulsarSessionArgs,
    },
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

/// Pulsar supervised connector (constructed from init context, no I/O).
#[derive(Clone)]
pub struct PulsarConnector {
    config: Arc<PulsarPluginConfig>,
    app_id: i32,
    app_name: String,
    runtime: Arc<dyn NorthwardRuntimeApi>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    retry_policy: RetryPolicy,
    downlink_routes: Option<Arc<DownlinkRouteTable>>,
}

impl PulsarConnector {
    /// Create the connector from init context (no I/O).
    pub fn from_init(ctx: NorthwardInitContext) -> NorthwardResult<Self> {
        let config = ctx
            .config
            .downcast_arc::<PulsarPluginConfig>()
            .map_err(|_| NorthwardError::ConfigurationError {
                message: "Failed to downcast to PulsarPluginConfig".to_string(),
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
impl Connector for PulsarConnector {
    type InitContext = NorthwardInitContext;
    type Handle = PulsarHandle;
    type Session = PulsarSession;

    #[inline]
    fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        Self::from_init(ctx)
    }

    async fn connect(
        &self,
        _ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let pulsar = connect_pulsar_client(&self.config.connection)
            .await
            .map_err(|e| NorthwardError::RuntimeError {
                reason: format!("pulsar client build failed: {e}"),
            })?;

        let producer = build_multi_topic_producer(&pulsar, &self.config.uplink.producer);

        // Attempt-scoped bounded queue for send-path side effects.
        let outbound_capacity = 100usize;
        let (outbound_tx, outbound_rx) = mpsc::channel(outbound_capacity);

        Ok(PulsarSession::new(PulsarSessionArgs {
            handle: Arc::new(PulsarHandle::new(
                Arc::clone(&self.config),
                self.app_id,
                self.app_name.clone(),
                Arc::clone(&self.runtime),
                outbound_tx,
            )),
            pulsar,
            producer,
            outbound_rx,
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
