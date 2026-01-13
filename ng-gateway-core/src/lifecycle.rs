//! Common lifecycle helpers for components that support a two-phase startup
//! (e.g. start + wait until connected).
//!
//! This module centralizes the shared `StartPolicy` enum and a small helper
//! function that encapsulates the common control-flow while allowing callers
//! to provide concrete async operations via closures. This avoids the need to
//! implement a trait on each actor/driver type and keeps call sites explicit.

use std::future::Future;

/// Start policy for components that establish a connection or long-running session.
///
/// This is shared between southward drivers and northward apps to keep behavior
/// consistent across the gateway.
#[derive(Clone, Copy, Debug)]
pub enum StartPolicy {
    /// Do not block, fire-and-forget.
    ///
    /// The manager is responsible for deciding whether the actual start
    /// operation should run in the background task or inline with best-effort
    /// error handling.
    AsyncFireAndForget,
    /// Configure + start, then synchronously wait until the component reaches
    /// a connected state (or timeout/fail).
    SyncWaitConnected { timeout_ms: u64 },
}

/// Apply a start policy to a pair of async operations provided by the caller.
///
/// This helper encapsulates the common control-flow shared by southward and
/// northward managers without requiring a dedicated trait on the underlying
/// component type.
///
/// The two operations are:
/// - `start_fn`: begins connection or background tasks.
/// - `wait_fn`: waits until the component reports a connected state.
///
/// # Type parameters
/// * `E`   - Error type.
/// * `SFut` - Future returned by `start_fn`.
/// * `WFut` - Future returned by `wait_fn`.
///
/// `start_fn` is required to be `'static` because the async-fire-and-forget
/// policy spawns it in a background task, while `wait_fn` can borrow local
/// state since it is only used in the synchronous branch.
pub async fn start_with_policy<E, SFut, WFut, SFn, WFn>(
    policy: StartPolicy,
    start_fn: SFn,
    wait_fn: WFn,
) -> Result<(), E>
where
    E: Send + 'static,
    SFn: FnOnce() -> SFut + Send + 'static,
    SFut: Future<Output = Result<(), E>> + Send + 'static,
    WFn: FnOnce(u64) -> WFut,
    WFut: Future<Output = Result<(), E>>,
{
    match policy {
        StartPolicy::AsyncFireAndForget => {
            tokio::spawn(async move {
                let _ = start_fn().await;
            });
            Ok(())
        }
        StartPolicy::SyncWaitConnected { timeout_ms } => {
            start_fn().await?;
            wait_fn(timeout_ms).await
        }
    }
}
