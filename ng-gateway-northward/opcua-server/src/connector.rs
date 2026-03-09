//! OPC UA Server supervised connector implementation.
//!
//! This plugin is an in-process OPC UA server. "Connect" means:
//! - bind/listen successfully
//! - initialize server runtime
//!
//! Connection governance (state publication, retries) is handled by SDK `SupervisorLoop`.

use super::{
    config::OpcuaServerPluginConfig, handle::OpcuaServerHandle, node_cache::NodeCache,
    queue::create_update_queue, server::OpcuaServerRuntime, session::OpcuaServerSession,
    write_dispatch::WriteDispatcher,
};
use ng_gateway_sdk::{
    log::fields as log_fields,
    supervision::{Connector, FailureKind, FailurePhase, Session, SessionContext},
    NorthwardError, NorthwardEvent, NorthwardInitContext, NorthwardResult, NorthwardRuntimeApi,
};
use std::{sync::Arc, time::Instant};
use tokio::sync::mpsc;
use tracing::info;

/// OPC UA Server connector.
#[derive(Clone)]
pub struct OpcuaServerConnector {
    config: Arc<OpcuaServerPluginConfig>,
    runtime: Arc<dyn NorthwardRuntimeApi>,
    /// App id (for log attribution and per-app overrides).
    app_id: i32,
    events_tx: mpsc::Sender<NorthwardEvent>,
}

impl OpcuaServerConnector {
    /// Create the connector from init context (no I/O).
    pub fn from_init(ctx: NorthwardInitContext) -> NorthwardResult<Self> {
        let config = ctx
            .config
            .downcast_arc::<OpcuaServerPluginConfig>()
            .map_err(|_| NorthwardError::ConfigurationError {
                message: "Failed to downcast to OpcuaServerPluginConfig".to_string(),
            })?;

        Ok(Self {
            config,
            runtime: ctx.runtime,
            app_id: ctx.app_id,
            events_tx: ctx.events_tx,
        })
    }
}

#[async_trait::async_trait]
impl Connector for OpcuaServerConnector {
    type InitContext = NorthwardInitContext;
    type Handle = OpcuaServerHandle;
    type Session = OpcuaServerSession;

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
        let _enter = ctx.span.enter();
        let t0 = Instant::now();
        info!(
            target: log_fields::TARGET_PLUGIN,
            attempt = ctx.attempt,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = self.app_id,
            host = %self.config.host,
            port = self.config.port,
            namespace_uri = %self.config.namespace_uri,
            "opcua-server connect: starting (build server + bind/listen)"
        );

        let (update_tx, update_rx) =
            create_update_queue(self.config.update_queue_capacity, self.config.drop_policy);
        let (node_build_tx, node_build_rx) = mpsc::channel::<i32>(4096);

        let node_cache = Arc::new(NodeCache::new());

        let handle = Arc::new(OpcuaServerHandle::new(
            Arc::clone(&self.config),
            Arc::clone(&self.runtime),
            Arc::clone(&node_cache),
            node_build_tx,
            update_tx,
            Arc::new(WriteDispatcher::new(
                Arc::clone(&self.config),
                Arc::clone(&self.runtime),
                Arc::clone(&node_cache),
                self.events_tx.clone(),
            )),
        ));

        // Starting the server is the "connect" step (bind/listen + runtime init).
        let t_server = Instant::now();
        let server = OpcuaServerRuntime::start(
            self.app_id,
            Arc::clone(&self.config),
            Arc::clone(&self.runtime),
            Arc::clone(&node_cache),
            Arc::clone(&handle.write_dispatch),
            ctx.cancel.child_token(),
        )
        .await?;
        info!(
            target: log_fields::TARGET_PLUGIN,
            attempt = ctx.attempt,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = self.app_id,
            server_start_ms = t_server.elapsed().as_millis() as u64,
            total_connect_ms = t0.elapsed().as_millis() as u64,
            "opcua-server connect: runtime ready"
        );

        Ok(OpcuaServerSession::new(
            handle,
            server,
            node_build_rx,
            update_rx,
        ))
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
