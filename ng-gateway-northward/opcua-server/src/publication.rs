//! Runtime publication channel between session and inspector.
//!
//! # Lifetime contract
//! `RuntimePublication` represents the *currently live* OPC UA server runtime
//! metadata exposed for low-frequency control-plane queries (inspector
//! snapshots, future health probes, etc.). It deliberately does **not** hold
//! any heavy resource handles (no `OpcuaServerRuntime`, no AddressSpace, no
//! cancellation token); doing so would make stale subscribers extend the life
//! of the underlying server through `Arc` strong refs and obscure ownership.
//!
//! Instead we publish a small, copyable metadata snapshot and rely on a RAII
//! guard (`RuntimePublicationGuard`) owned by the active `Session` to keep the
//! `Some(publication)` window aligned with the Session's `init()`..`drop()`
//! lifetime. When the session ends (cancellation, fatal failure, normal
//! disconnect), the guard's `Drop` clears the channel back to `None` so any
//! late inspector call observes a "no live runtime" state instead of stale
//! data.
//!
//! # Concurrency
//! `RuntimePublisher` is wrapped in `Arc<watch::Sender<...>>` so the connector
//! can (a) hand a publisher clone to each session attempt and (b) keep its own
//! reference for diagnostic resets. `tokio::sync::watch::Sender::send(&self)`
//! is internally synchronized, making concurrent guard drops well-defined.

use crate::pki::CertSummary;
use std::sync::Arc;
use tokio::sync::watch;

/// Lightweight, value-typed publication for a live OPC UA server runtime.
///
/// # Design notes
/// - Owns no `Arc<...>` to anywhere in the OPC UA server stack.
/// - `Clone` is cheap (small `Vec<String>` + a small `CertSummary`).
/// - The inspector takes a snapshot via `watch::Receiver::borrow()` and
///   clones the inner publication.
#[derive(Debug, Clone)]
pub struct RuntimePublication {
    /// Effective namespace index registered into the AddressSpace.
    pub namespace_index: u16,
    /// Operator-configured local TCP bind address.
    pub bind_addr: String,
    /// Operator-configured advertised endpoint URLs (post-validation).
    pub advertised_endpoints: Vec<String>,
    /// Live certificate summary; `None` if PKI bring-up failed silently.
    pub cert_summary: Option<CertSummary>,
}

/// Shared sender side; multiple holders may publish (only the latest wins).
pub type RuntimePublisher = Arc<watch::Sender<Option<RuntimePublication>>>;

/// Cheap-to-clone subscriber side used by the inspector.
pub type RuntimeSubscriber = watch::Receiver<Option<RuntimePublication>>;

/// Allocate a publication channel.
///
/// The initial value is `None`; the first session to publish via
/// `RuntimePublicationGuard::publish` will flip it to `Some(...)`.
#[inline]
pub fn channel() -> (RuntimePublisher, RuntimeSubscriber) {
    let (tx, rx) = watch::channel::<Option<RuntimePublication>>(None);
    (Arc::new(tx), rx)
}

/// RAII guard that publishes a `RuntimePublication` for its lifetime.
///
/// On `Drop` it clears the slot back to `None`. The session embeds one of
/// these so any path that ends the session (return, `?`, panic unwind) leaves
/// no stale publication behind.
#[must_use = "RuntimePublicationGuard must be retained for the session lifetime; \
              dropping it immediately clears the publication slot"]
pub struct RuntimePublicationGuard {
    publisher: RuntimePublisher,
}

impl RuntimePublicationGuard {
    /// Publish `publication` and capture a clearing-on-drop guard.
    ///
    /// Sending may fail only when no receivers exist; in that case the channel
    /// is effectively unobservable so we silently discard the error rather
    /// than failing session bring-up.
    #[inline]
    pub fn publish(publisher: RuntimePublisher, publication: RuntimePublication) -> Self {
        let _ = publisher.send(Some(publication));
        Self { publisher }
    }
}

impl Drop for RuntimePublicationGuard {
    #[inline]
    fn drop(&mut self) {
        let _ = self.publisher.send(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> RuntimePublication {
        RuntimePublication {
            namespace_index: 1,
            bind_addr: "0.0.0.0:4840".into(),
            advertised_endpoints: vec!["opc.tcp://localhost:4840/".into()],
            cert_summary: None,
        }
    }

    #[tokio::test]
    async fn guard_publishes_on_construct_and_clears_on_drop() {
        let (publisher, mut subscriber) = channel();
        assert!(subscriber.borrow().is_none());

        {
            let _guard = RuntimePublicationGuard::publish(Arc::clone(&publisher), fresh());
            // Mark as observed so the next `changed()` waits for a real edge.
            subscriber.mark_changed();
            assert!(subscriber.borrow_and_update().is_some());
        }

        // Guard dropped: subscriber should observe the clearing send.
        subscriber.changed().await.expect("publisher still alive");
        assert!(subscriber.borrow().is_none());
    }

    #[tokio::test]
    async fn replacing_guard_overwrites_publication() {
        let (publisher, mut subscriber) = channel();
        let g1 = RuntimePublicationGuard::publish(Arc::clone(&publisher), fresh());

        let other = RuntimePublication {
            namespace_index: 2,
            bind_addr: "0.0.0.0:4841".into(),
            advertised_endpoints: vec!["opc.tcp://other:4840/".into()],
            cert_summary: None,
        };
        let _g2 = RuntimePublicationGuard::publish(Arc::clone(&publisher), other.clone());

        subscriber.mark_changed();
        let observed = subscriber
            .borrow_and_update()
            .clone()
            .expect("must be Some");
        assert_eq!(observed.namespace_index, other.namespace_index);
        drop(g1); // Dropping the older guard MUST clear regardless of order.
        subscriber.changed().await.unwrap();
        assert!(subscriber.borrow().is_none());
    }
}
