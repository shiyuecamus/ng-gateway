//! Supervision protocol traits: `Connector` and `Session`.
//!
//! This layer is implemented by southward drivers / northward plugins to plug into the unified
//! lifecycle controller (`SupervisorLoop`).
//!
//! # Key idea
//! - `Connector::connect()` establishes a session (transport + protocol handshake).
//! - `Session::init()` performs post-connect initialization that defines "Ready".
//! - `Session::run()` drives the session until disconnect/cancel/reconnect request.

use super::{super::CollectorConcurrencyProfile, FailureKind, FailurePhase, FailureReport};
use crate::{NorthwardError, NorthwardResult};
use std::{error, sync::Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::Span;

/// A cheap reconnect request handle injected into a session/handle.
///
/// Implementations MUST call `try_request_reconnect` (never `await`) to avoid blocking hot paths.
#[derive(Clone, Debug)]
pub struct ReconnectHandle {
    tx: mpsc::Sender<Arc<str>>,
}

impl ReconnectHandle {
    #[inline]
    pub(crate) fn new(tx: mpsc::Sender<Arc<str>>) -> Self {
        Self { tx }
    }

    /// Best-effort reconnect request.
    ///
    /// Returns `true` if the request was enqueued, `false` otherwise.
    #[inline]
    pub fn try_request_reconnect(&self, reason: impl Into<Arc<str>>) -> bool {
        self.tx.try_send(reason.into()).is_ok()
    }
}

/// Session execution outcome that the supervisor loop uses to decide the next phase.
#[derive(Debug)]
pub enum RunOutcome {
    /// Session terminated normally (treated as disconnected; may reconnect depending on policy).
    Disconnected,
    /// Session requests an immediate reconnect (without treating it as a failure).
    ReconnectRequested(Arc<str>),
    /// Session reported a classified fatal failure (stop reconnecting).
    Fatal(FailureReport),
}

/// Context injected into `Connector` and `Session` calls.
///
/// This contains control-plane signals and must be cheap to clone.
#[derive(Clone)]
pub struct SessionContext {
    /// Global cancellation for the supervised component.
    pub cancel: CancellationToken,
    /// Reconnect request handle (best-effort, bounded).
    pub reconnect: ReconnectHandle,
    /// A tracing span that contains stable labels (e.g., channel_id/app_id, type).
    ///
    /// Implementations should create child spans from this span when needed.
    pub span: Span,
    /// Supervision attempt counter (monotonic, starts from 1).
    pub attempt: u64,
}

impl SessionContext {
    /// Create a fatal failure report with a summary string.
    #[inline]
    pub fn fatal(
        &self,
        phase: FailurePhase,
        summary: impl Into<Arc<str>>,
        code: Option<Arc<str>>,
    ) -> FailureReport {
        FailureReport {
            phase,
            kind: FailureKind::Fatal,
            summary: summary.into(),
            code,
        }
    }
}

/// Establish a `Session`.
#[async_trait::async_trait]
pub trait Connector: Send + Sync + 'static {
    /// Initialization context type for constructing this connector.
    type InitContext: Send + 'static;
    /// Data-plane handle type to publish when Ready.
    type Handle: Send + Sync + 'static;
    /// Session type that drives the connection lifecycle.
    type Session: Session<Handle = Self::Handle>;

    /// Construct the connector from initialization context.
    ///
    /// This is invoked by `ng_driver_factory!` / `ng_plugin_factory!` macro
    /// expansions on behalf of the host.
    ///
    /// # Async runtime semantics
    ///
    /// The returned future is **always polled on the plugin/driver's own
    /// `NG_RUNTIME`**. The macro-generated factory wraps the host call in
    /// `RuntimeAware{Plugin,Driver}Factory`, which:
    ///
    /// - spawns the inner future onto the plugin's runtime via
    ///   [`tokio::runtime::Handle::spawn`] (so the plugin's tokio TLS is
    ///   available throughout this future), and
    /// - guards the resulting [`tokio::task::JoinHandle`] with
    ///   [`tokio_util::task::AbortOnDropHandle`] so a host-side drop
    ///   (API timeout, request abort, parent cancellation) immediately
    ///   aborts this construction at its next `.await` point.
    ///
    /// Concretely, implementations may freely use any tokio primitive
    /// without manual runtime hops:
    ///
    /// - [`tokio::spawn`] for connector-scoped background tasks (cert
    ///   monitors, periodic refreshers, internal mailboxes, …),
    /// - [`tokio::time`] / [`tokio::fs`] / [`tokio::sync::*`] freely,
    /// - [`tokio::task::spawn_blocking`] (`.await` it) for CPU-bound
    ///   bootstrap such as RSA keygen, large config decoding, etc.
    ///
    /// # Cancellation contract
    ///
    /// This future MUST be cancel-safe. Any partial state created before
    /// an `.await` (temporary files, spawned tasks, allocated channels,
    /// background mailboxes) MUST be reclaimed via `Drop`. The recommended
    /// pattern is to bind such state to RAII guards (e.g. a struct that
    /// cancels a [`tokio_util::sync::CancellationToken`] in its `Drop`
    /// implementation) and only commit them into `Self` once construction
    /// is fully successful.
    ///
    /// # Where to do heavy work
    ///
    /// Heavy or one-shot bootstrap work belongs **here**, not in
    /// [`Connector::connect`]. The supervision `connect()` window is
    /// constrained by the host's `start_app_with_policy` timeout; anything
    /// that lives for the entire connector lifetime (PKI bootstrap,
    /// long-lived monitors, schema registries, persistent caches) should
    /// be initialised here so the supervision retry/backoff cycle stays
    /// tight and predictable.
    async fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized;

    /// Provide a best-effort, allocation-free concurrency capability profile **before**
    /// the data-plane handle becomes available.
    ///
    /// # Default
    /// Strict serialization (`serial()`).
    #[inline]
    fn collector_concurrency_profile_hint(&self) -> CollectorConcurrencyProfile {
        CollectorConcurrencyProfile::serial()
    }

    /// Establish the session (transport + protocol-level connect).
    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error>;

    /// Invoke a plugin/driver-specific low-frequency capability.
    ///
    /// # Notes
    /// This method is control-plane only and defaults to a stable "not
    /// supported" error so SDK-supervised components can expose optional
    /// runtime introspection without changing the hot-path data contracts.
    async fn invoke_capability(
        &self,
        capability_id: &str,
        _request: serde_json::Value,
    ) -> NorthwardResult<serde_json::Value> {
        Err(NorthwardError::CapabilityNotSupported {
            capability_id: capability_id.to_string(),
        })
    }

    /// Classify an error as retryable/fatal/stop depending on the phase.
    ///
    /// # Notes
    /// - `Connect/Init/Run` may have different semantics.
    /// - This drives retry/budget decisions in `SupervisorLoop`.
    fn classify_error(
        &self,
        phase: FailurePhase,
        err: &<Self::Session as Session>::Error,
    ) -> FailureKind;

    /// Build a UI/alert-friendly summary string for an error.
    #[inline]
    fn error_summary(&self, err: &<Self::Session as Session>::Error) -> Arc<str> {
        Arc::<str>::from(err.to_string())
    }

    /// Optional stable error code for aggregation/alerting.
    #[inline]
    fn error_code(&self, _err: &<Self::Session as Session>::Error) -> Option<Arc<str>> {
        None
    }
}

/// A single connected session lifecycle.
#[async_trait::async_trait]
pub trait Session: Send + 'static {
    /// Data-plane handle type to publish when Ready.
    type Handle: Send + Sync + 'static;
    /// Error type produced by this session.
    type Error: error::Error + Send + Sync + 'static;

    /// The data-plane handle is always published as `Arc<Handle>`.
    ///
    /// This makes handle publish O(1) and prevents ambiguous "owned/clone" contracts.
    fn handle(&self) -> &Arc<Self::Handle>;

    /// Perform post-connect initialization that defines "Ready".
    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error>;

    /// Drive the session until disconnect/cancel/reconnect.
    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error>;
}
