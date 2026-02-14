//! Ethernet/IP southward data-plane handle.
//!
//! This is the **only** hot-path object published by the SDK supervision loop.
//! It wraps an `EipSessionPool` of multiple CIP sessions and provides:
//! - batched tag reads distributed across the pool (uplink)
//! - tag writes (downlink)

use super::{
    codec::EthernetIpCodec,
    types::{EthernetIpChannel, EthernetIpDevice, EthernetIpParameter, EthernetIpPoint},
};
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{FuturesUnordered, StreamExt};
use ng_gateway_sdk::{
    supervision::ReconnectHandle, AccessMode, CollectItem, CollectionGroupKey,
    CollectorConcurrencyProfile, DeviceBuffers, DriverError, DriverResult, ExecuteOutcome,
    ExecuteResult, NGValue, NorthwardData, PointValue, RuntimeAction, RuntimeDelta, RuntimeDevice,
    RuntimeParameter, RuntimePoint, SouthwardHandle, WriteOutcome, WriteResult,
};
use rust_ethernet_ip::EipClient;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    time::Duration as StdDuration,
};
use tokio::{sync::Mutex, time::timeout};
use tracing::{error, warn};

/// Connection pool for Ethernet/IP CIP sessions.
///
/// Each entry is an independent TCP + CIP session guarded by a `Mutex`
/// (single-flight per connection). Concurrency is achieved by distributing
/// batches across multiple connections using lock-free round-robin selection.
pub struct EipSessionPool {
    /// Pool of CIP clients, each behind a mutex for single-flight protection.
    clients: Vec<Arc<Mutex<EipClient>>>,
    /// Per-member health flag. `true` = healthy (default), `false` = suspected dead.
    healthy: Vec<AtomicBool>,
    /// Lock-free round-robin counter for connection selection.
    rr: AtomicUsize,
}

impl EipSessionPool {
    /// Create a new pool from a list of connected `EipClient` instances.
    pub fn new(clients: Vec<EipClient>) -> Self {
        let n = clients.len();
        Self {
            clients: clients
                .into_iter()
                .map(|c| Arc::new(Mutex::new(c)))
                .collect(),
            healthy: (0..n).map(|_| AtomicBool::new(true)).collect(),
            rr: AtomicUsize::new(0),
        }
    }

    /// Round-robin pick a healthy client from the pool.
    ///
    /// Skips members marked as unhealthy. Falls back to any member if all are
    /// unhealthy (better to try than to fail immediately).
    #[inline]
    pub fn pick(&self) -> Option<Arc<Mutex<EipClient>>> {
        let n = self.clients.len();
        if n == 0 {
            return None;
        }
        let base = self.rr.fetch_add(1, Ordering::Relaxed);
        // First pass: prefer healthy members.
        for offset in 0..n {
            let i = (base + offset) % n;
            if self.healthy[i].load(Ordering::Relaxed) {
                return Some(Arc::clone(&self.clients[i]));
            }
        }
        // Fallback: all unhealthy, pick by round-robin anyway.
        let i = base % n;
        Some(Arc::clone(&self.clients[i]))
    }

    /// Mark a pool member as unhealthy after a transport error.
    #[inline]
    pub fn mark_unhealthy(&self, client: &Arc<Mutex<EipClient>>) {
        for (i, c) in self.clients.iter().enumerate() {
            if Arc::ptr_eq(c, client) {
                self.healthy[i].store(false, Ordering::Relaxed);
                return;
            }
        }
    }

    /// Number of connections in the pool.
    #[inline]
    pub fn pool_size(&self) -> usize {
        self.clients.len()
    }
}

impl std::fmt::Debug for EipSessionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EipSessionPool")
            .field("pool_size", &self.clients.len())
            .finish()
    }
}

/// Ethernet/IP data-plane handle.
pub struct EthernetIpHandle {
    inner: Arc<EthernetIpChannel>,
    /// Session pool (lock-free hot reads via ArcSwap).
    pool: ArcSwapOption<EipSessionPool>,
    reconnect: OnceLock<ReconnectHandle>,
    /// Consecutive timeout counter — reconnect only after exceeding `config.max_timeouts`.
    consecutive_timeouts: AtomicU32,
}

impl EthernetIpHandle {
    /// ASCII: "ENCH"
    const KIND_ETH_CHANNEL: u32 = 0x454E_4348;

    #[inline]
    pub fn new(inner: Arc<EthernetIpChannel>) -> Self {
        Self {
            inner,
            pool: ArcSwapOption::from(None),
            reconnect: OnceLock::new(),
            consecutive_timeouts: AtomicU32::new(0),
        }
    }

    #[inline]
    pub(crate) fn set_reconnect(&self, reconnect: ReconnectHandle) {
        let _ = self.reconnect.set(reconnect);
    }

    /// Attach a connected session pool for this attempt.
    #[inline]
    pub(crate) fn attach_pool(&self, pool: Arc<EipSessionPool>) {
        self.pool.store(Some(pool));
    }

    /// Detach session pool and return the previous pool for graceful shutdown.
    #[inline]
    pub(crate) fn detach_pool(&self) -> Option<Arc<EipSessionPool>> {
        self.pool.swap(None)
    }

    #[inline]
    fn try_request_reconnect(&self, reason: &'static str) {
        if let Some(h) = self.reconnect.get() {
            let _ = h.try_request_reconnect(reason);
        }
    }

    #[inline]
    fn load_pool(&self) -> DriverResult<Arc<EipSessionPool>> {
        self.pool.load_full().ok_or(DriverError::ServiceUnavailable)
    }

    /// Effective pool size (clamped to configured limits).
    pub(crate) fn effective_pool_size(&self) -> usize {
        self.inner.config.pool_size.clamp(1, 32) as usize
    }
}

#[async_trait]
impl SouthwardHandle for EthernetIpHandle {
    fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        device
            .downcast_ref::<EthernetIpDevice>()
            .map(|d| CollectionGroupKey::from_u64(Self::KIND_ETH_CHANNEL, d.channel_id as u64))
    }

    #[inline]
    fn collector_concurrency_profile(&self) -> CollectorConcurrencyProfile {
        CollectorConcurrencyProfile::concurrent(self.effective_pool_size())
    }

    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        if items.is_empty() {
            return Err(DriverError::ValidationError(
                "collect_data called with empty items".to_string(),
            ));
        }

        let mut buffers = HashMap::with_capacity(items.len());
        let mut points = Vec::new();

        for (dev_any, points_any) in items.iter() {
            let dev = dev_any.downcast_ref::<EthernetIpDevice>().ok_or(
                DriverError::ConfigurationError(
                    "RuntimeDevice is not EthernetIpDevice".to_string(),
                ),
            )?;

            buffers
                .entry(dev.id)
                .or_insert_with(|| DeviceBuffers::new(dev.device_name.clone()));

            for p_any in points_any.iter() {
                let Ok(p) = Arc::clone(p_any).downcast_arc::<EthernetIpPoint>() else {
                    continue;
                };
                if !matches!(p.access_mode, AccessMode::Read | AccessMode::ReadWrite) {
                    continue;
                }
                points.push(p);
            }
        }

        if points.is_empty() {
            return Ok(Vec::new());
        }

        let pool = self.load_pool()?;
        let batch_size = self.inner.config.batch_size.max(1) as usize;
        let timeout_ms = self.inner.config.timeout;

        // Execute read batches concurrently across the session pool using
        // Semaphore + FuturesUnordered. Unlike wave-based execution, completed
        // batches immediately free a concurrency slot for the next batch,
        // eliminating sync-point stalls.
        let concurrency = pool.pool_size().clamp(1, 8);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let batches: Vec<&[Arc<EthernetIpPoint>]> = points.chunks(batch_size).collect();

        let mut futs = FuturesUnordered::new();
        for (idx, chunk) in batches.iter().enumerate() {
            let permit = semaphore.clone().acquire_owned().await.map_err(|_| {
                DriverError::ExecutionError("EIP concurrency semaphore closed".into())
            })?;
            let client_mutex = pool.pick();
            let chunk = *chunk;
            futs.push(async move {
                let _permit = permit; // held for the duration of this batch
                let client_mutex = client_mutex.ok_or(DriverError::ServiceUnavailable)?;
                let tag_names: Vec<&str> = chunk.iter().map(|p| p.tag_name.as_str()).collect();
                let op_res = timeout(StdDuration::from_millis(timeout_ms), async {
                    let mut client = client_mutex.lock().await;
                    client.read_tags_batch(&tag_names).await
                })
                .await;
                Ok::<_, DriverError>((idx, client_mutex, op_res))
            });
        }

        let mut overall_success = true;
        while let Some(r) = futs.next().await {
            let (idx, client_ref, op_res) = match r {
                Ok(v) => v,
                Err(_) => {
                    overall_success = false;
                    continue;
                }
            };
            let chunk = batches[idx];
            match op_res {
                Ok(Ok(results)) => {
                    // Success — reset consecutive timeout counter.
                    self.consecutive_timeouts.store(0, Ordering::Relaxed);
                    // Match results by tag_name for safety, not by position index.
                    let result_map: HashMap<&str, _> = results
                        .iter()
                        .map(|(name, res)| (name.as_str(), res))
                        .collect();

                    if result_map.len() != chunk.len() {
                        warn!(
                            expected = chunk.len(),
                            actual = result_map.len(),
                            "read_tags_batch returned mismatched result count"
                        );
                    }

                    for point in chunk.iter() {
                        let Some(res) = result_map.get(point.tag_name.as_str()) else {
                            warn!(tag = %point.tag_name, "Tag missing from batch read response");
                            continue;
                        };
                        let Some(buf) = buffers.get_mut(&point.device_id) else {
                            continue;
                        };
                        match res {
                            Ok(plc_value) => {
                                match EthernetIpCodec::to_ng_value(
                                    plc_value.clone(),
                                    point.logical_data_type(),
                                    &point.transform,
                                ) {
                                    Ok(val) => {
                                        buf.push(
                                            point.r#type,
                                            PointValue {
                                                point_id: point.id,
                                                point_key: Arc::from(point.key.as_str()),
                                                value: val,
                                            },
                                        );
                                    }
                                    Err(e) => {
                                        warn!("Codec error for point {}: {}", point.tag_name, e);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Error reading point {}: {}", point.tag_name, e);
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    // Transport error — always reconnect immediately.
                    warn!("Batch read failed: {}", e);
                    overall_success = false;
                    pool.mark_unhealthy(&client_ref);
                    self.consecutive_timeouts.store(0, Ordering::Relaxed);
                    self.try_request_reconnect("ethernetip batch read failed");
                }
                Err(_) => {
                    // Timeout — use consecutive counter.
                    overall_success = false;
                    pool.mark_unhealthy(&client_ref);
                    let count = self.consecutive_timeouts.fetch_add(1, Ordering::Relaxed) + 1;
                    let threshold = self.inner.config.max_timeouts.max(1);
                    if count >= threshold {
                        warn!(
                            consecutive_timeouts = count,
                            threshold, "Batch read timeout threshold reached, request reconnect"
                        );
                        self.consecutive_timeouts.store(0, Ordering::Relaxed);
                        self.try_request_reconnect("ethernetip timeout threshold reached");
                    } else {
                        warn!(
                            consecutive_timeouts = count,
                            threshold, "Batch read timeout (below reconnect threshold)"
                        );
                    }
                }
            }
        }

        let any_data = buffers.values().any(|b| !b.is_empty());
        if !overall_success && !any_data {
            return Err(DriverError::ExecutionError(
                "All batch reads failed".to_string(),
            ));
        }

        let ts = Utc::now();
        let mut device_ids: Vec<i32> = buffers.keys().copied().collect();
        device_ids.sort_unstable();
        let mut out = Vec::with_capacity(device_ids.len() * 2);
        for device_id in device_ids {
            let Some(buf) = buffers.remove(&device_id) else {
                continue;
            };
            out.extend(buf.into_northward(device_id, ts));
        }
        Ok(out)
    }

    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        _action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let _device =
            device
                .downcast_ref::<EthernetIpDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not EthernetIpDevice".to_string(),
                ))?;

        if parameters.is_empty() {
            return Err(DriverError::ValidationError(
                "No parameters provided for write".into(),
            ));
        }

        let pool = self.load_pool()?;
        let client_mutex = pool.pick().ok_or(DriverError::ServiceUnavailable)?;
        let mut results = Vec::with_capacity(parameters.len());
        let mut overall_success = true;

        // Pre-encode all values before acquiring the lock to minimise hold time.
        let mut write_items: Vec<(&str, _)> = Vec::with_capacity(parameters.len());
        for (param, value) in parameters.iter() {
            let eth_param = param.downcast_ref::<EthernetIpParameter>().ok_or(
                DriverError::ConfigurationError("Invalid Parameter Type".into()),
            )?;
            if eth_param.tag_name.is_empty() {
                warn!("Parameter {} has no tag_name, skipping", eth_param.name);
                continue;
            }
            let plc_value = EthernetIpCodec::to_plc_value(value, eth_param.wire_data_type())?;
            write_items.push((&eth_param.tag_name, plc_value));
        }

        // Acquire the client lock once for the entire batch of writes,
        // avoiding N acquire/release cycles that would contend with concurrent reads.
        let op_res = timeout(StdDuration::from_millis(self.inner.config.timeout), async {
            let mut client = client_mutex.lock().await;
            for (tag_name, plc_value) in &write_items {
                match client.write_tag(tag_name, plc_value.clone()).await {
                    Ok(_) => results.push(format!("Wrote to {tag_name}")),
                    Err(e) => {
                        error!("Write tag {tag_name} failed: {e}");
                        overall_success = false;
                    }
                }
            }
        })
        .await;

        if op_res.is_err() {
            overall_success = false;
            self.try_request_reconnect("ethernetip write timeout");
        }

        if !overall_success {
            return Err(DriverError::ExecutionError(
                "One or more writes failed".into(),
            ));
        }

        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(json!({"status": "success", "details": results})),
        })
    }

    async fn write_point(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let point =
            point
                .downcast_ref::<EthernetIpPoint>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimePoint is not EthernetIpPoint".to_string(),
                ))?;

        let pool = self.load_pool()?;
        let client_mutex = pool.pick().ok_or(DriverError::ServiceUnavailable)?;
        let plc_value = EthernetIpCodec::to_plc_value(value, point.wire_data_type())?;
        let timeout_dur = StdDuration::from_millis(timeout_ms.unwrap_or(self.inner.config.timeout));

        let op_res = timeout(timeout_dur, async {
            let mut client = client_mutex.lock().await;
            client.write_tag(&point.tag_name, plc_value).await
        })
        .await;

        match op_res {
            Ok(Ok(_)) => Ok(WriteResult {
                outcome: WriteOutcome::Applied,
                applied_value: Some(value.clone()),
            }),
            Ok(Err(e)) => Err(DriverError::ExecutionError(e.to_string())),
            Err(_) => Err(DriverError::Timeout(timeout_dur)),
        }
    }

    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        Ok(())
    }
}
