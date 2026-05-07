//! OPC UA Server supervised connector.
//!
//! # Lifecycle ownership
//! The connector owns long-lived, cross-attempt resources:
//! - `NodeCache` — preserves point→NodeId bindings across reconnects so the
//!   inspector keeps reporting the materialized AddressSpace even while the
//!   underlying server is rebinding.
//! - `RuntimePublisher` — control-plane channel exposed to the inspector. Each
//!   attempt's session republishes its live runtime metadata via a RAII guard
//!   (see `crate::publication`).
//! - `OpcuaServerInspector` — handles `inspector:v1` capability calls.
//! - **Cert-expiry monitor** — spawned during construction; it runs until the
//!   connector's `monitor_lifecycle` RAII guard drops (when the last `Arc`
//!   strong count for that guard reaches zero).
//!
//! # Why not let the session own the inspector?
//! Inspector calls must succeed in any phase (pre-connect, connecting,
//! reconnecting, failed). Hosting the inspector at connector level makes its
//! availability orthogonal to session liveness; the inspector simply reports
//! whether a publication is live.
//!
//! # Async runtime semantics
//! `Connector::new` is now asynchronous and is polled on the plugin's own
//! `NG_RUNTIME` (the SDK FFI wrapper guarantees this hop). This unlocks
//! three correctness improvements over the previous design:
//!
//! 1.  **PKI bootstrap on the plugin's blocking pool** —
//!     `pki::prepare_for_runtime` may take O(seconds) on a fresh app
//!     because of RSA-2048 keygen. We push it onto
//!     [`tokio::task::spawn_blocking`] so that work runs on the plugin's
//!     dedicated blocking pool instead of monopolising any tokio worker.
//! 2.  **Eager monitor spawn** —
//!     The cert-expiry monitor is spawned right here in construction with
//!     `tokio::spawn`, eliminating the previous `AtomicBool` lazy-spawn
//!     dance inside `connect()`.
//! 3.  **Cancel-safe construction** —
//!     If the host drops the construction future (API timeout, parent
//!     cancellation), the SDK wrapper aborts this future at its next
//!     `.await` point. Any partial state we already built (the spawned
//!     monitor task, the publisher channel, …) is owned by `MonitorLifecycle`
//!     and `Self`, both of which are RAII-clean.

use super::{
    config::OpcuaServerPluginConfig,
    handle::OpcuaServerHandle,
    inspector::OpcuaServerInspector,
    node_cache::NodeCache,
    pki::{self, CertIntent, CertSummary},
    protocol::validate_advertised_endpoints,
    publication::{self, RuntimePublisher},
    queue::create_update_queue,
    server::{OpcuaServerRuntime, OpcuaServerRuntimeStartParams},
    session::OpcuaServerSession,
    write_dispatch::WriteDispatcher,
};
use ng_gateway_sdk::{
    log::fields as log_fields,
    northward::opcua_server::CAPABILITY_INSPECTOR_V1,
    supervision::{Connector, FailureKind, FailurePhase, Session, SessionContext},
    NorthwardError, NorthwardEvent, NorthwardInitContext, NorthwardResult, NorthwardRuntimeApi,
};
use std::{path::PathBuf, sync::Arc, time::Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, Instrument};

/// RAII guard for connector-scoped background tasks (currently the
/// cert-expiry monitor).
///
/// The guard is held by the connector inside an [`Arc`]; it cancels its
/// [`CancellationToken`] when the **last clone** of the connector goes
/// away (i.e. when the supervisor loop shuts down). The spawned monitor
/// task observes the cancellation through a child token and exits cleanly.
///
/// Keeping the lifecycle separate from the rest of the connector makes
/// the cancellation contract explicit, makes the connector trivially
/// `Clone`-friendly, and removes the need for any atomic guard around
/// "did I spawn the monitor yet?" — that is now a static fact of
/// construction.
struct MonitorLifecycle {
    token: CancellationToken,
}

impl Drop for MonitorLifecycle {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

/// OPC UA Server connector — connector-level ownership of long-lived resources.
#[derive(Clone)]
pub struct OpcuaServerConnector {
    config: Arc<OpcuaServerPluginConfig>,
    runtime: Arc<dyn NorthwardRuntimeApi>,
    /// App id (for log attribution and per-app overrides).
    app_id: i32,
    events_tx: mpsc::Sender<NorthwardEvent>,
    /// Connector-owned NodeId cache, shared across supervision attempts.
    node_cache: Arc<NodeCache>,
    /// Publisher half of the runtime-publication channel; shared with sessions.
    publisher: RuntimePublisher,
    /// Low-frequency capability inspector.
    inspector: Arc<OpcuaServerInspector>,
    /// PKI directory used by both the connector (for the expiry monitor) and
    /// the runtime start sequence.
    pki_dir: PathBuf,
    /// Live application instance certificate summary, materialised once during
    /// construction.
    ///
    /// This is **the** authoritative description of the on-disk certificate
    /// the OPC UA server will load. It is kept at connector scope so:
    /// - reconnect attempts reuse it without re-running the synchronous PKI
    ///   reconcile pipeline,
    /// - the supervision `connect()` path stays cheap and well within the
    ///   host-side `start_timeout_ms` budget,
    /// - the inspector capability can surface a stable summary regardless of
    ///   whether the runtime is currently bound or rebinding.
    cert_summary: Arc<CertSummary>,
    /// **Mandatory retention (do not remove).** Holds one strong reference to
    /// [`MonitorLifecycle`] so the parent [`CancellationToken`] outlives
    /// construction and is only dropped when this connector (and any
    /// [`Clone`]s sharing the same `Arc`) are released. The monitor task uses
    /// [`CancellationToken::child_token`]; when this guard triggers
    /// [`MonitorLifecycle::drop`], the monitor exits its loop.
    ///
    /// This field is never read by methods; it exists solely for ownership and
    /// `Drop` order. `#[allow(dead_code)]` keeps the intent explicit for
    /// reviewers—this is not dead code.
    #[allow(dead_code)]
    monitor_lifecycle: Arc<MonitorLifecycle>,
}

impl OpcuaServerConnector {
    /// Build the connector from initialisation context.
    ///
    /// # Async semantics
    /// This future is polled on the plugin's own `NG_RUNTIME` (guaranteed by
    /// the SDK FFI wrapper). It is therefore safe to call `tokio::spawn`,
    /// `tokio::task::spawn_blocking`, etc. directly.
    ///
    /// # What this function does
    /// 1. Downcasts and validates the configuration (cheap, sync).
    /// 2. Validates the advertised-endpoint list and derives the
    ///    [`CertIntent`] (cheap, sync).
    /// 3. **Off-loads** the entire PKI reconcile/generate/load pipeline to
    ///    [`tokio::task::spawn_blocking`] — this is the work that may take
    ///    O(seconds) on a fresh `app_id` because of RSA-2048 keygen.
    /// 4. Materialises the connector-scoped resources (`NodeCache`,
    ///    publisher channel, inspector).
    /// 5. **Spawns** the cert-expiry monitor on the plugin runtime, bound
    ///    to a [`MonitorLifecycle`] RAII guard so the task is cancelled
    ///    automatically when the connector is dropped.
    ///
    /// # Cancellation
    /// Cancel-safe at every `.await`:
    /// - If we are aborted before `spawn_blocking` returns, the blocking
    ///   task continues to completion in the background but its result is
    ///   discarded (acceptable: PKI work is idempotent).
    /// - If we are aborted after spawning the monitor, dropping the
    ///   half-built connector also drops `MonitorLifecycle`, which cancels
    ///   the monitor task cleanly.
    pub async fn from_init(ctx: NorthwardInitContext) -> NorthwardResult<Self> {
        let config = ctx
            .config
            .downcast_arc::<OpcuaServerPluginConfig>()
            .map_err(|_| NorthwardError::ConfigurationError {
                message: "Failed to downcast to OpcuaServerPluginConfig".to_string(),
            })?;

        // Validate the advertised-endpoint list and derive the certificate
        // intent. Failing here surfaces misconfiguration BEFORE the
        // supervisor goes through its connect/backoff machinery, which
        // improves UX for create-app requests by funnelling the error into
        // the same API response that already drives the create flow.
        let advertised = validate_advertised_endpoints(&config.advertised_endpoints)?;
        let intent = CertIntent::new(&config.application_uri, &advertised);

        let pki_dir = PathBuf::from(format!("./pki/plugin/{}", ctx.app_id));
        let app_name_for_cert = format!("NG-Gateway OPC UA Server (app {})", ctx.app_id);
        let app_id = ctx.app_id;

        // Off-load the synchronous PKI reconcile/generate/load pipeline to
        // the plugin's blocking pool. RSA-2048 keygen + X509 self-sign
        // takes O(seconds) on weak hardware / debug builds; we MUST NOT
        // hold a tokio worker for that duration.
        //
        // Subsequent calls (reconnects, restarts) hit the `KeepExisting`
        // fast path and complete in milliseconds — but we still pay the
        // (small) `spawn_blocking` overhead, which is negligible compared
        // to the rest of the supervision lifecycle.
        let cert_summary = {
            let pki_dir = pki_dir.clone();
            let product_uri = config.product_uri.clone();
            let warn_days = config.cert_expiry_warn_days;
            let intent = intent.clone();
            tokio::task::spawn_blocking(move || {
                pki::prepare_for_runtime(
                    &pki_dir,
                    &app_name_for_cert,
                    &product_uri,
                    &intent,
                    warn_days,
                    app_id,
                )
            })
            .await
            .map_err(|join_err| NorthwardError::RuntimeError {
                reason: if join_err.is_panic() {
                    format!("PKI bootstrap task panicked: {join_err}")
                } else {
                    format!("PKI bootstrap task cancelled: {join_err}")
                },
            })??
        };

        let node_cache = Arc::new(NodeCache::new());
        let (publisher, subscriber) = publication::channel();
        let inspector = Arc::new(OpcuaServerInspector::new(
            Arc::clone(&config),
            Arc::clone(&ctx.runtime),
            Arc::clone(&node_cache),
            subscriber,
        ));

        // Spawn the cert-expiry monitor right here. The lifecycle guard
        // cancels the parent token on `Drop`, which propagates to the
        // child token held by the spawned task.
        let monitor_token = CancellationToken::new();
        let monitor_span = tracing::info_span!(
            target: log_fields::TARGET_PLUGIN,
            "opcua-server-cert-monitor",
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = i64::from(app_id),
        );
        tokio::spawn(
            pki::run_expiry_monitor(
                pki_dir.clone(),
                config.application_uri.clone(),
                config.cert_expiry_warn_days,
                app_id,
                monitor_token.child_token(),
            )
            .instrument(monitor_span),
        );

        Ok(Self {
            config,
            runtime: ctx.runtime,
            app_id,
            events_tx: ctx.events_tx,
            node_cache,
            publisher,
            inspector,
            pki_dir,
            cert_summary: Arc::new(cert_summary),
            monitor_lifecycle: Arc::new(MonitorLifecycle {
                token: monitor_token,
            }),
        })
    }
}

#[async_trait::async_trait]
impl Connector for OpcuaServerConnector {
    type InitContext = NorthwardInitContext;
    type Handle = OpcuaServerHandle;
    type Session = OpcuaServerSession;

    #[inline]
    async fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        Self::from_init(ctx).await
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
            bind_addr = %self.config.bind_addr,
            advertised_count = self.config.advertised_endpoints.len(),
            namespace_uri = %self.config.namespace_uri,
            pki_dir = %self.pki_dir.display(),
            cert_thumbprint = %self.cert_summary.thumbprint_hex,
            "opcua-server connect: starting (build server + bind/listen)"
        );

        let (update_tx, update_rx) =
            create_update_queue(self.config.update_queue_capacity, self.config.drop_policy);
        let (node_build_tx, node_build_rx) = mpsc::channel::<i32>(4096);

        let handle = Arc::new(OpcuaServerHandle::new(
            Arc::clone(&self.config),
            Arc::clone(&self.runtime),
            Arc::clone(&self.node_cache),
            node_build_tx,
            update_tx,
            Arc::new(WriteDispatcher::new(
                Arc::clone(&self.config),
                Arc::clone(&self.runtime),
                Arc::clone(&self.node_cache),
                self.events_tx.clone(),
            )),
        ));

        let t_server = Instant::now();
        let server = OpcuaServerRuntime::start(OpcuaServerRuntimeStartParams {
            app_id: self.app_id,
            config: Arc::clone(&self.config),
            runtime: Arc::clone(&self.runtime),
            node_cache: Arc::clone(&self.node_cache),
            write_dispatch: Arc::clone(&handle.write_dispatch),
            cert_summary: Arc::clone(&self.cert_summary),
            pki_dir: self.pki_dir.clone(),
            shutdown: ctx.cancel.child_token(),
        })
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
            Arc::clone(&self.publisher),
        ))
    }

    async fn invoke_capability(
        &self,
        capability_id: &str,
        request: serde_json::Value,
    ) -> NorthwardResult<serde_json::Value> {
        match capability_id {
            CAPABILITY_INSPECTOR_V1 => self.inspector.handle(request).await,
            other => Err(NorthwardError::CapabilityNotSupported {
                capability_id: other.to_string(),
            }),
        }
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
