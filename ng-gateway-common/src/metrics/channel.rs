//! Instrumented Tokio channels for queue/backpressure observability.
//!
//! This module provides a small wrapper around `tokio::sync::mpsc` that:
//! - Tracks queue depth using a cheap atomic counter (best-effort).
//! - Records dropped items with bounded, low-cardinality reasons.
//! - Observes send-side blocked time (for backpressure visibility).
//!
//! # Design notes
//! - Metrics are registered via `NGMetricsHub` and `metrics::queue`.
//! - This module is intentionally small and allocation-free on hot paths.
//! - The counter is best-effort and does not provide strict ordering guarantees.

use crate::metrics::{
    queue::{DropReason, QueueObserverInner},
    NGMetricsHub,
};
use ng_gateway_error::NGResult;
use std::{
    fmt,
    sync::{atomic::Ordering, Arc},
    time::Instant,
};
use tokio::sync::mpsc::{
    self,
    error::{SendError, SendTimeoutError, TrySendError},
};

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
pub fn bounded<T>(
    metrics_hub: &NGMetricsHub,
    queue_name: impl Into<String>,
    capacity: usize,
) -> NGResult<(InstrumentedSender<T>, InstrumentedReceiver<T>)> {
    let queue_name = queue_name.into();
    let capacity_u64 = capacity as u64;

    let observer = metrics_hub
        .queue()
        .register_queue(queue_name, capacity_u64)?;
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

/// Backward-compatible alias for `bounded`.
#[deprecated(note = "use `ng_gateway_common::channel::bounded()` instead")]
#[inline]
pub fn channel<T>(
    metrics_hub: &NGMetricsHub,
    queue_name: impl Into<String>,
    capacity: usize,
) -> NGResult<(InstrumentedSender<T>, InstrumentedReceiver<T>)> {
    bounded(metrics_hub, queue_name, capacity)
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
    pub fn new(
        metrics_hub: &NGMetricsHub,
        queue_name: impl Into<String>,
        capacity: u64,
    ) -> NGResult<Self> {
        Ok(Self {
            inner: metrics_hub
                .queue()
                .register_queue(queue_name.into(), capacity)?,
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
            .field("capacity", &self.capacity())
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
        self.observer.metrics.capacity.get().max(0) as u64
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
    pub async fn send(&self, value: T) -> Result<(), SendError<T>> {
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
    ) -> Result<(), SendTimeoutError<T>> {
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
            Err(SendTimeoutError::Timeout(v)) => {
                self.observer.metrics.inc_dropped(DropReason::Timeout);
                Err(SendTimeoutError::Timeout(v))
            }
            Err(SendTimeoutError::Closed(v)) => {
                self.observer.metrics.inc_dropped(DropReason::Closed);
                Err(SendTimeoutError::Closed(v))
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
            .field("capacity", &self.observer.metrics.capacity.get().max(0))
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
