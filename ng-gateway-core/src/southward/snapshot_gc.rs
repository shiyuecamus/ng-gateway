//! Southward device snapshot GC (best-effort).
//!
//! This module implements a bounded, best-effort GC mechanism for `DeviceDataSnapshot`:
//! it periodically evicts point baselines whose `(now_ms - last_touch_ms) > ttl_ms`.
//!
//! # Design notes
//! - Control-plane / maintenance only: this is not on the hot data path.
//! - Bounded work per tick: limits CPU and lock contention.
//! - Non-blocking workers: use bounded mpsc and `try_send` to avoid unbounded memory.

use super::{DeviceDataSnapshot, SnapshotGcRuntime};
use crate::southward::internal::snapshot_now_ms;
use dashmap::DashMap;
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::sync::mpsc;
use tracing::Instrument;

const SNAPSHOT_GC_MAX_WORKERS: usize = 16;

/// Best-effort GC for a single device snapshot.
///
/// This function is synchronous and intended to run in a Tokio task without `.await`.
fn gc_device_snapshot_points(
    device_snapshots: &DashMap<i32, DeviceDataSnapshot>,
    device_id: i32,
    now_ms: u64,
    ttl_ms: u64,
) {
    let Some(mut snap) = device_snapshots.get_mut(&device_id) else {
        return;
    };

    snap.telemetry
        .retain(|_, (ts, _)| now_ms.saturating_sub(*ts) <= ttl_ms);
    snap.client_attributes
        .retain(|_, (ts, _)| now_ms.saturating_sub(*ts) <= ttl_ms);
    snap.shared_attributes
        .retain(|_, (ts, _)| now_ms.saturating_sub(*ts) <= ttl_ms);
    snap.server_attributes
        .retain(|_, (ts, _)| now_ms.saturating_sub(*ts) <= ttl_ms);

    // Keep `point_key_by_id` best-effort: drop keys not present in any map.
    // This avoids unbounded growth when points are evicted.
    if !snap.point_key_by_id.is_empty() {
        // Split borrows across fields to satisfy the borrow checker.
        let DeviceDataSnapshot {
            telemetry,
            client_attributes,
            shared_attributes,
            server_attributes,
            point_key_by_id,
            ..
        } = &mut *snap;
        point_key_by_id.retain(|pid, _| {
            telemetry.contains_key(pid)
                || client_attributes.contains_key(pid)
                || shared_attributes.contains_key(pid)
                || server_attributes.contains_key(pid)
        });
    }

    // If everything is empty, remove the whole snapshot to free memory.
    if snap.telemetry.is_empty()
        && snap.client_attributes.is_empty()
        && snap.shared_attributes.is_empty()
        && snap.server_attributes.is_empty()
    {
        drop(snap);
        let _ = device_snapshots.remove(&device_id);
    }
}

impl SnapshotGcRuntime {
    /// Start background snapshot GC tasks (idempotent).
    ///
    /// # What this does
    /// - Periodically scans a bounded number of device snapshots
    /// - Evicts point baselines whose `(now_ms - last_touch_ms) > ttl_ms`
    ///
    /// # Notes
    /// - This is best-effort: it bounds memory growth but does not guarantee immediate eviction.
    /// - If `ttl_ms == 0`, GC is disabled.
    pub(crate) fn start(&self, device_snapshots: Arc<DashMap<i32, DeviceDataSnapshot>>) {
        // Idempotent start.
        if self
            .started
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let cfg = Arc::clone(&self.cfg);

        // Worker queues (bounded) to avoid unbounded memory.
        let mut senders: Vec<mpsc::Sender<i32>> = Vec::with_capacity(SNAPSHOT_GC_MAX_WORKERS);
        for _wid in 0..SNAPSHOT_GC_MAX_WORKERS {
            let (tx, mut rx) = mpsc::channel::<i32>(1024);
            senders.push(tx);

            let device_snapshots = Arc::clone(&device_snapshots);
            let shutdown = self.shutdown.child_token();
            let cfg = Arc::clone(&cfg);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => {
                            break;
                        }
                        maybe_id = rx.recv() => {
                            let Some(device_id) = maybe_id else {
                                break;
                            };
                            let ttl_ms = cfg.device_change_cache_ttl_ms();
                            if ttl_ms == 0 {
                                continue;
                            }
                            let now_ms = snapshot_now_ms();
                            gc_device_snapshot_points(&device_snapshots, device_id, now_ms, ttl_ms);
                        }
                    }
                }
            }
            .in_current_span());
        }

        // Dispatcher: pick up to N device ids and distribute to workers by hash partition.
        //
        // This makes `gc_workers` semantics explicit: each worker owns a shard of devices, reducing
        // cross-worker contention on the same `device_id`.
        let shutdown = self.shutdown.child_token();
        let cfg = Arc::clone(&cfg);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(cfg.snapshot_gc_interval_ms().max(200))) => {
                        let ttl_ms = cfg.device_change_cache_ttl_ms();
                        if ttl_ms == 0 {
                            continue;
                        }
                        let workers = cfg.snapshot_gc_workers().clamp(1, SNAPSHOT_GC_MAX_WORKERS);
                        let max_devices_per_tick = cfg.max_devices_per_snapshot_tick().max(1);
                        let mut n = 0usize;
                        for e in device_snapshots.iter() {
                            let device_id = *e.key();
                            // Stable partition: same device_id always goes to same worker.
                            // Best-effort: if queue is full, skip.
                            let idx = (device_id as u32 as usize) % workers;
                            let _ = senders[idx].try_send(device_id);
                            n += 1;
                            if n >= max_devices_per_tick {
                                break;
                            }
                        }
                    }
                }
            }
        }
        .in_current_span());
    }
}
