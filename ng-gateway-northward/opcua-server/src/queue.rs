use crate::config::DropPolicy;
use chrono::{DateTime, Utc};
use ng_gateway_sdk::PointValue;
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::{Mutex, Notify};

#[derive(Debug, Clone)]
pub enum UpdateKind {
    Telemetry,
    Attributes,
}

#[derive(Debug, Clone)]
pub struct UpdateBatch {
    #[allow(dead_code)]
    pub timestamp: DateTime<Utc>,
    #[allow(dead_code)]
    pub kind: UpdateKind,
    pub values: Arc<[PointValue]>,
}

#[derive(Clone)]
pub struct UpdateQueueTx {
    inner: Arc<QueueInner>,
}

pub struct UpdateQueueRx {
    inner: Arc<QueueInner>,
}

struct QueueInner {
    buf: Mutex<VecDeque<UpdateBatch>>,
    not_empty: Notify,
    not_full: Notify,
    capacity: usize,
    drop_policy: DropPolicy,
}

pub fn create_update_queue(
    capacity: usize,
    drop_policy: DropPolicy,
) -> (UpdateQueueTx, UpdateQueueRx) {
    let inner = Arc::new(QueueInner {
        buf: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
        not_empty: Notify::new(),
        not_full: Notify::new(),
        capacity,
        drop_policy,
    });
    (
        UpdateQueueTx {
            inner: Arc::clone(&inner),
        },
        UpdateQueueRx { inner },
    )
}

impl UpdateQueueTx {
    /// Fast path enqueue with backpressure policy.
    ///
    /// - `discard_oldest`: drop the oldest batch, keep the newest.
    /// - `discard_newest`: drop current batch.
    /// - `block_with_timeout`: return Err; caller may choose to await with timeout.
    pub fn try_enqueue(&self, batch: UpdateBatch) -> Result<(), ()> {
        // Best-effort non-blocking enqueue:
        // if lock is contended, signal backpressure to keep `process_data()` fast.
        let Ok(mut guard) = self.inner.buf.try_lock() else {
            return Err(());
        };

        if guard.len() < self.inner.capacity {
            guard.push_back(batch);
            drop(guard);
            self.inner.not_empty.notify_one();
            return Ok(());
        }

        match self.inner.drop_policy {
            DropPolicy::DiscardNewest => Err(()),
            DropPolicy::DiscardOldest => {
                let _ = guard.pop_front();
                guard.push_back(batch);
                drop(guard);
                self.inner.not_empty.notify_one();
                Ok(())
            }
            DropPolicy::BlockWithTimeout => Err(()),
        }
    }

    pub async fn enqueue_blocking(&self, batch: UpdateBatch) -> Result<(), ()> {
        loop {
            let mut guard = self.inner.buf.lock().await;
            if guard.len() < self.inner.capacity {
                guard.push_back(batch);
                drop(guard);
                self.inner.not_empty.notify_one();
                return Ok(());
            }
            drop(guard);
            self.inner.not_full.notified().await;
        }
    }
}

impl UpdateQueueRx {
    pub async fn recv(&mut self) -> Option<UpdateBatch> {
        loop {
            let mut guard = self.inner.buf.lock().await;
            if let Some(v) = guard.pop_front() {
                drop(guard);
                self.inner.not_full.notify_one();
                return Some(v);
            }
            drop(guard);
            self.inner.not_empty.notified().await;
        }
    }
}
