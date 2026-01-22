//! Instrumented bounded `tokio::sync::mpsc` channels for backpressure observability.
//!
//! This module provides a high-quality, production-oriented wrapper around Tokio bounded mpsc
//! channels that exposes **low-cardinality** Prometheus metrics for:
//! - queue depth / capacity
//! - drops (full / timeout / closed / buffer_full / expired)
//! - blocked time (when producers wait for capacity)
//!
//! # Design principles
//! - **Hot path is atomic only**: depth changes update an `AtomicU64` only.
//! - **Scrape-time gauge refresh**: Prometheus gauges are refreshed right before encoding.
//! - **Low cardinality by default**: queue identity is a bounded string set (do not use device/point).
//! - **No unwrap/expect**: all errors are handled and converted into metrics + returned errors.

use std::{
    fmt,
    sync::{atomic::Ordering, Arc},
    time::Instant,
};

pub use crate::metrics::queue::DropReason;
use crate::metrics::queue::{register_queue, QueueObserverInner};
use ng_gateway_error::NGResult;
use tokio::sync::mpsc::{self, error::TrySendError};

/// Create a new bounded instrumented channel.
///
/// # Arguments
/// - `queue_name`: A stable, low-cardinality queue name (e.g. `collector_outbound`)
/// - `capacity`: Bounded channel capacity
///
/// # Notes
/// - The returned sender/receiver share the same depth counter.
/// - Metrics are registered (best-effort) on first creation for this `queue_name`.
#[inline]
pub fn channel<T>(
    queue_name: impl Into<String>,
    capacity: usize,
) -> NGResult<(InstrumentedSender<T>, InstrumentedReceiver<T>)> {
    let queue_name = queue_name.into();
    let capacity_u64 = capacity as u64;

    let observer = register_queue(queue_name, capacity_u64)?;
    let (tx, rx) = mpsc::channel::<T>(capacity);

    Ok((
        InstrumentedSender {
            inner: tx,
            observer: Arc::clone(&observer),
        },
        InstrumentedReceiver {
            inner: rx,
            observer,
        },
    ))
}

/// A non-mpsc queue observer for instrumenting custom buffers (e.g. VecDeque buffer).
///
/// This is useful for bounded buffers that are not implemented as Tokio channels but still need
/// the same queue observability contract.
#[derive(Clone)]
pub struct QueueObserver {
    inner: Arc<QueueObserverInner>,
}

impl QueueObserver {
    /// Register a queue observer with a fixed capacity.
    #[inline]
    pub fn new(queue_name: impl Into<String>, capacity: u64) -> NGResult<Self> {
        Ok(Self {
            inner: register_queue(queue_name.into(), capacity)?,
        })
    }

    /// Increment current depth (best-effort saturating).
    #[inline]
    pub fn inc(&self) {
        self.inner.depth.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement current depth (best-effort saturating).
    #[inline]
    pub fn dec(&self) {
        let prev = self.inner.depth.fetch_sub(1, Ordering::Relaxed);
        if prev == 0 {
            // Saturate at 0 on underflow.
            self.inner.depth.store(0, Ordering::Relaxed);
        }
    }

    /// Record a dropped item with a bounded reason label.
    #[inline]
    pub fn dropped(&self, reason: DropReason) {
        self.inner.metrics.inc_dropped(reason);
    }
}

/// Sender wrapper that instruments enqueue-side behavior.
#[derive(Clone)]
pub struct InstrumentedSender<T> {
    inner: mpsc::Sender<T>,
    observer: Arc<QueueObserverInner>,
}

impl<T> fmt::Debug for InstrumentedSender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstrumentedSender")
            .field("queue", &self.observer.queue_name)
            .field("capacity", &self.observer.capacity)
            .field("depth", &self.len())
            .finish()
    }
}

impl<T> InstrumentedSender<T> {
    /// Get a cheap, best-effort current depth.
    #[inline]
    pub fn len(&self) -> u64 {
        self.observer.depth.load(Ordering::Relaxed)
    }

    /// Returns `true` if the current depth is zero.
    ///
    /// # Notes
    /// This is a cheap, best-effort check based on an atomic counter and does not provide any
    /// strict ordering guarantees under concurrency.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the configured capacity.
    #[inline]
    pub fn capacity(&self) -> u64 {
        self.observer.capacity
    }

    /// Try to enqueue without blocking.
    ///
    /// On saturation, this increments `ng_gateway_queue_dropped_total{reason="full"}`.
    #[inline]
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        match self.inner.try_send(value) {
            Ok(()) => {
                self.observer.depth.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Full(v)) => {
                self.observer.metrics.inc_dropped(DropReason::Full);
                Err(TrySendError::Full(v))
            }
            Err(TrySendError::Closed(v)) => {
                self.observer.metrics.inc_dropped(DropReason::Closed);
                Err(TrySendError::Closed(v))
            }
        }
    }

    /// Enqueue with backpressure (await until there is capacity).
    ///
    /// This observes blocked time in `ng_gateway_queue_blocked_seconds`.
    pub async fn send(&self, value: T) -> Result<(), mpsc::error::SendError<T>> {
        let start = Instant::now();
        let res = self.inner.send(value).await;
        self.observer
            .metrics
            .observe_blocked_seconds(start.elapsed().as_secs_f64());

        match res {
            Ok(()) => {
                self.observer.depth.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => {
                self.observer.metrics.inc_dropped(DropReason::Closed);
                Err(e)
            }
        }
    }

    /// Enqueue with backpressure and a timeout budget.
    ///
    /// This is intended for `DropPolicy::Block` style semantics.
    /// - On timeout, increments `dropped_total{reason="timeout"}`.
    /// - Always observes blocked time (until completion/timeout).
    pub async fn send_timeout(
        &self,
        value: T,
        timeout: std::time::Duration,
    ) -> Result<(), mpsc::error::SendTimeoutError<T>> {
        let start = Instant::now();
        let res = self.inner.send_timeout(value, timeout).await;
        self.observer
            .metrics
            .observe_blocked_seconds(start.elapsed().as_secs_f64());

        match res {
            Ok(()) => {
                self.observer.depth.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(mpsc::error::SendTimeoutError::Timeout(v)) => {
                self.observer.metrics.inc_dropped(DropReason::Timeout);
                Err(mpsc::error::SendTimeoutError::Timeout(v))
            }
            Err(mpsc::error::SendTimeoutError::Closed(v)) => {
                self.observer.metrics.inc_dropped(DropReason::Closed);
                Err(mpsc::error::SendTimeoutError::Closed(v))
            }
        }
    }
}

/// Receiver wrapper that instruments dequeue-side behavior.
pub struct InstrumentedReceiver<T> {
    inner: mpsc::Receiver<T>,
    observer: Arc<QueueObserverInner>,
}

impl<T> fmt::Debug for InstrumentedReceiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstrumentedReceiver")
            .field("queue", &self.observer.queue_name)
            .field("capacity", &self.observer.capacity)
            .field("depth", &self.observer.depth.load(Ordering::Relaxed))
            .finish()
    }
}

impl<T> InstrumentedReceiver<T> {
    /// Receive the next value from the channel.
    ///
    /// When an item is received, the queue depth is decremented.
    pub async fn recv(&mut self) -> Option<T> {
        let item = self.inner.recv().await;
        if item.is_some() {
            let prev = self.observer.depth.fetch_sub(1, Ordering::Relaxed);
            if prev == 0 {
                // Saturate at 0 on underflow.
                self.observer.depth.store(0, Ordering::Relaxed);
            }
        }
        item
    }
}
