use crate::{southward::SouthwardDataBus, NGSouthwardManager};
use arc_swap::ArcSwap;
use chrono::Utc;
use futures::{future::join_all, StreamExt};
use ng_gateway_common::metrics::{
    southward::{CollectBatchOutcome, SouthwardChannelMetricHandles},
    NGMetricsHub,
};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{core::metrics::CollectorMetricsSnapshot, settings::Collector};
use ng_gateway_sdk::{
    CollectItem, CollectionGroupKey, Driver, NorthwardData, RetryController, RetryDecision,
    RetryPolicy, RuntimeDevice, RuntimePoint,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{RwLock, Semaphore},
    task::JoinHandle,
    time::{interval, sleep, timeout, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn, Instrument};

/// A flattened device entry used by the collector hot path.
///
/// The collector first builds a list of device entries, then re-groups them by
/// `(driver instance, group key)` to maximize batching and throughput.
#[derive(Clone)]
struct DeviceEntry {
    device_id: i32,
    runtime_device: Arc<dyn RuntimeDevice>,
    points: Arc<[Arc<dyn RuntimePoint>]>,
}

/// A tuple containing the driver handle and a list of device entries.
type DeviceEntries = (Option<Arc<dyn Driver>>, Vec<DeviceEntry>);

/// A single driver group call containing a batch of collect items.
///
/// Each group should share the same physical session semantics (e.g. Modbus slave ID)
/// so the southward driver can execute the batch efficiently.
#[derive(Clone)]
struct GroupCall {
    items: Vec<CollectItem>,
}

#[derive(Debug)]
struct GroupCollectError {
    _err: NGError,
    is_timeout: bool,
}

/// Context for a group collection execution
struct GroupCollectContext {
    total_budget: Duration,
    retry_policy: RetryPolicy,
    semaphore: Arc<ArcSwap<Semaphore>>,
    metrics_hub: Arc<NGMetricsHub>,
    prom: Option<Arc<SouthwardChannelMetricHandles>>,
    token: CancellationToken,
}

/// Collection engine with enhanced performance and reliability
#[derive(Clone)]
pub struct NGCollector {
    /// Engine configuration
    config: Collector,
    /// Channel manager reference
    southward_manager: Arc<NGSouthwardManager>,
    /// Metrics hub (single source of truth).
    metrics_hub: Arc<NGMetricsHub>,
    /// Collection tasks by channel ID
    collection_tasks: Arc<RwLock<HashMap<i32, JoinHandle<()>>>>,
    /// Master cancellation token
    master_token: CancellationToken,
    /// Channel-specific cancellation tokens
    channel_tokens: Arc<RwLock<HashMap<i32, CancellationToken>>>,
    /// Fixed semaphore for bounded concurrency
    semaphore: Arc<ArcSwap<Semaphore>>,
    /// Best-effort total permits (for metrics & debugging).
    semaphore_limit: Arc<AtomicUsize>,
    /// Data batch sender (bounded)
    southward_data_bus: Arc<SouthwardDataBus>,
}

impl NGCollector {
    #[inline]
    async fn refresh_active_tasks_metric(&self) {
        // Best-effort: number of per-channel collection tasks currently registered.
        let n = self.collection_tasks.read().await.len() as u64;
        self.metrics_hub.set_collector_active_tasks(n);
    }

    #[inline]
    /// Create a new collection engine
    pub fn new(
        config: Collector,
        southward_manager: Arc<NGSouthwardManager>,
        metrics_hub: Arc<NGMetricsHub>,
        southward_data_bus: Arc<SouthwardDataBus>,
        master_token: CancellationToken,
    ) -> Self {
        let max_concurrency = config.max_concurrent_collections();
        Self {
            config,
            southward_manager,
            metrics_hub,
            collection_tasks: Arc::new(RwLock::new(HashMap::new())),
            master_token,
            channel_tokens: Arc::new(RwLock::new(HashMap::new())),
            semaphore: Arc::new(ArcSwap::from_pointee(Semaphore::new(max_concurrency))),
            semaphore_limit: Arc::new(AtomicUsize::new(max_concurrency)),
            southward_data_bus,
        }
    }

    /// Hot-apply max concurrent collections (global).
    ///
    /// This swaps the semaphore instance for future acquisitions. In-flight group calls
    /// continue using their already-acquired permits.
    #[inline]
    pub fn set_max_concurrent_collections(&self, max: usize) {
        let max = max.max(1);
        self.semaphore_limit.store(max, Ordering::Relaxed);
        self.semaphore.store(Arc::new(Semaphore::new(max)));
    }

    #[instrument(name = "collection-start", skip_all)]
    /// Start the collection engine
    pub async fn start(&self) -> NGResult<()> {
        info!("🚀 Starting collection engine");

        // Create channel-specific cancellation tokens
        let channel_ids = self.southward_manager.get_collectable_channels();
        for channel_id in &channel_ids {
            let channel_token = self.master_token.child_token();
            self.channel_tokens
                .write()
                .await
                .insert(*channel_id, channel_token);
        }

        // Start collection tasks in parallel for better performance
        let startup_tasks: Vec<_> = channel_ids
            .into_iter()
            .map(|channel_id| {
                let collector = self.clone();
                tokio::spawn(async move { collector.start_channel_collection(channel_id).await })
            })
            .collect();

        // Wait for all channels to start
        let results = join_all(startup_tasks).await;
        let mut successful = 0;
        let mut failed = 0;

        for result in results {
            match result {
                Ok(Ok(_)) => {
                    successful += 1;
                }
                Ok(Err(e)) => {
                    failed += 1;
                    error!(error=%e, "❌ Failed to start channel collection");
                }
                Err(e) => {
                    failed += 1;
                    error!(error=%e, "❌ Channel collection task panicked");
                }
            }
        }

        info!("📊 Collection engine started: {successful} successful, {failed} failed channels");

        // Update metrics (prefer actual registered tasks, not "successful starts").
        self.refresh_active_tasks_metric().await;

        if successful == 0 {
            warn!("No channels started successfully");
        }
        Ok(())
    }

    #[instrument(name = "channel-collection-start", skip_all)]
    /// Start collection for a specific channel
    async fn start_channel_collection(&self, channel_id: i32) -> NGResult<()> {
        let channel =
            self.southward_manager
                .get_channel(channel_id)
                .ok_or(NGError::InitializationError(format!(
                    "Channel {channel_id} not found"
                )))?;

        if !channel.config.collectable() {
            return Ok(());
        }

        debug!("🔄 Starting collection for channel id: [{channel_id}]");

        let channel_token = self
            .channel_tokens
            .read()
            .await
            .get(&channel_id)
            .cloned()
            .ok_or(NGError::InitializationError(format!(
                "No token for channel {channel_id}"
            )))?;

        let period_ms = channel.config.period().unwrap_or(10000);
        let southward_manager = Arc::clone(&self.southward_manager);
        let metrics_hub = Arc::clone(&self.metrics_hub);
        let config = self.config.clone();
        let semaphore = Arc::clone(&self.semaphore);
        let southward_data_bus = Arc::clone(&self.southward_data_bus);
        let channel_name = channel.config.name().to_string();
        let driver_id = channel.config.driver_id();

        // Bind a per-channel span so logs can be attributed to `channel_id` without
        // requiring every log callsite to manually include the field.
        let span = tracing::info_span!(
            "southward-channel",
            channel_id = channel_id,
            channel_name = %channel_name,
            driver_id = driver_id
        );

        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(period_ms as u64));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let channel_name = channel_name.clone();

            // Add jitter to prevent thundering herd effect
            let jitter =
                Duration::from_millis((channel_id as u64 * 37) % (period_ms as u64 / 10).max(1));
            sleep(jitter).await;

            info!(
                "✅ Channel [{channel_name}] collection task started with {period_ms}ms interval"
            );

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = Self::collect_channel_data(
                            channel_id,
                            &channel_name,
                            &southward_manager,
                            &metrics_hub,
                            &config,
                            &semaphore,
                            &southward_data_bus,
                            &channel_token,
                        ).await {
                            error!(error=%e, "❌ Failed to collect data for channel [{channel_name}]");
                        }
                    }
                    _ = channel_token.cancelled() => {
                        info!("📨 Channel [{channel_name}] collection cancelled");
                        break;
                    }
                }
            }
        }.instrument(span));

        self.collection_tasks.write().await.insert(channel_id, task);

        Ok(())
    }

    /// Build device entries for one channel tick, ensuring a single driver instance.
    ///
    /// # Notes
    /// Devices without points are skipped.
    #[inline]
    fn build_device_entries(
        southward_manager: &Arc<NGSouthwardManager>,
        device_ids: Vec<i32>,
    ) -> NGResult<DeviceEntries> {
        let mut driver_handle = None;
        let mut entries = Vec::with_capacity(device_ids.len());

        for device_id in device_ids.into_iter() {
            let (driver, runtime_device) = {
                let dev = southward_manager
                    .get_device(device_id)
                    .ok_or(NGError::Error(format!("Device {} not found", device_id)))?;
                (dev.driver.clone(), dev.config.clone())
            };

            let points = southward_manager.get_device_points(device_id);
            if points.is_empty() {
                continue;
            }

            if let Some(existing) = &driver_handle {
                debug_assert!(
                    Arc::ptr_eq(existing, &driver),
                    "Collector observed multiple driver instances in one channel tick"
                );
            } else {
                driver_handle = Some(Arc::clone(&driver));
            }

            entries.push(DeviceEntry {
                device_id,
                runtime_device,
                points,
            });
        }

        Ok((driver_handle, entries))
    }

    /// Build driver group calls from device entries.
    ///
    /// # Performance
    /// This keeps the batching strategy in one place: first group by driver-provided key,
    /// then emit singleton calls for devices without a key.
    #[inline]
    fn build_group_calls(driver: &Arc<dyn Driver>, entries: Vec<DeviceEntry>) -> Vec<GroupCall> {
        let mut groups = Vec::new();

        // Second-level grouping by driver-provided group key.
        let mut keyed: HashMap<CollectionGroupKey, Vec<DeviceEntry>> = HashMap::new();
        let mut singles: Vec<DeviceEntry> = Vec::new();

        for e in entries.into_iter() {
            match driver.collection_group_key(e.runtime_device.as_ref()) {
                Some(k) => keyed.entry(k).or_default().push(e),
                None => singles.push(e),
            }
        }

        for (_k, mut vec) in keyed {
            vec.sort_by_key(|e| e.device_id);
            let items: Vec<CollectItem> = vec
                .into_iter()
                .map(|e| (e.runtime_device, e.points))
                .collect();
            if !items.is_empty() {
                groups.push(GroupCall { items });
            }
        }

        for e in singles.into_iter() {
            groups.push(GroupCall {
                items: vec![(e.runtime_device, e.points)],
            });
        }

        groups
    }

    /// Record one collector cycle with unified success/fail/timeout semantics.
    #[inline]
    fn record_collector_cycle(
        metrics_hub: &NGMetricsHub,
        ok: bool,
        is_timeout: bool,
        elapsed_ns: u64,
        elapsed_seconds: f64,
    ) {
        if ok {
            metrics_hub.record_collector_cycle_success(elapsed_ns, elapsed_seconds);
        } else if is_timeout {
            metrics_hub.record_collector_cycle_timeout(elapsed_ns, elapsed_seconds);
        } else {
            metrics_hub.record_collector_cycle_fail(elapsed_ns, elapsed_seconds);
        }
    }

    /// Execute one driver group call with cancellation + semaphore bounding.
    async fn collect_one_group(
        driver: Arc<dyn Driver>,
        group: GroupCall,
        ctx: GroupCollectContext,
    ) -> Result<Vec<NorthwardData>, GroupCollectError> {
        // Make semaphore acquire cancellable (owned permit).
        let sem = ctx.semaphore.load_full();
        let acquired = tokio::select! {
            _ = ctx.token.cancelled() => {
                return Err(GroupCollectError {
                    _err: NGError::Error("Group collection cancelled".to_string()),
                    is_timeout: false,
                });
            }
            p = sem.acquire_owned() => p.map_err(|_| GroupCollectError {
                _err: NGError::Error("Semaphore closed".to_string()),
                is_timeout: false,
            }),
        }?;
        let _permit = acquired;

        let total_budget = if ctx.total_budget.is_zero() {
            Duration::from_millis(1)
        } else {
            ctx.total_budget
        };

        let started = Instant::now();
        let budget_ms = total_budget.as_millis().min(u64::MAX as u128) as u64;
        let mut policy = ctx.retry_policy;
        policy.max_elapsed_time_ms = match (policy.max_elapsed_time_ms, Some(budget_ms)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (None, Some(b)) => Some(b),
            (Some(a), None) => Some(a),
            (None, None) => None,
        };
        let mut retry = RetryController::new(&policy);

        loop {
            let elapsed = started.elapsed();
            if elapsed >= total_budget {
                return Err(GroupCollectError {
                    _err: NGError::Error("Group collection timeout budget exhausted".to_string()),
                    is_timeout: true,
                });
            }
            let remaining = total_budget.saturating_sub(elapsed);

            // One attempt = one driver call (preserve batching semantics).
            let io_start = Instant::now();
            let (res, is_timeout) = tokio::select! {
                _ = ctx.token.cancelled() => {
                    return Err(GroupCollectError {
                        _err: NGError::Error("Group collection cancelled".to_string()),
                        is_timeout: false,
                    });
                }
                r = timeout(remaining, driver.collect_data(&group.items)) => {
                    match r {
                        Ok(inner) => (inner.map_err(|e| NGError::DriverError(e.to_string())), false),
                        Err(_) => (Err(NGError::Error("Group collection timeout".to_string())), true),
                    }
                }
            };

            let call_elapsed = io_start.elapsed();
            let elapsed_ns = call_elapsed.as_nanos().min(u64::MAX as u128) as u64;
            let elapsed_seconds = call_elapsed.as_secs_f64();
            let points_read = group
                .items
                .iter()
                .map(|(_dev, pts)| pts.len() as u64)
                .sum::<u64>();

            let ok = res.is_ok();
            if let Some(h) = &ctx.prom {
                let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                h.record_collect_batch(
                    group.items.iter().map(|(dev, _pts)| dev.id()),
                    CollectBatchOutcome {
                        ok,
                        is_timeout,
                        points: points_read,
                        elapsed_ns,
                        elapsed_seconds,
                        now_ms,
                    },
                );
            }

            if let Ok(v) = res {
                Self::record_collector_cycle(
                    &ctx.metrics_hub,
                    true,
                    false,
                    elapsed_ns,
                    elapsed_seconds,
                );
                return Ok(v);
            }

            let err = match res {
                Ok(v) => return Ok(v),
                Err(e) => e,
            };

            let retryable = is_timeout || matches!(err, NGError::DriverError(_));
            if !retryable {
                Self::record_collector_cycle(
                    &ctx.metrics_hub,
                    false,
                    is_timeout,
                    elapsed_ns,
                    elapsed_seconds,
                );
                return Err(GroupCollectError {
                    _err: err,
                    is_timeout,
                });
            }

            match retry.on_failure() {
                RetryDecision::RetryAfter(delay) => {
                    if is_timeout {
                        ctx.metrics_hub.inc_collector_retries_timeout();
                    } else {
                        ctx.metrics_hub.inc_collector_retries_error();
                    }

                    let elapsed2 = started.elapsed();
                    if elapsed2 >= total_budget {
                        return Err(GroupCollectError {
                            _err: err,
                            is_timeout: true,
                        });
                    }
                    let remaining2 = total_budget.saturating_sub(elapsed2);
                    let actual_sleep = std::cmp::min(delay, remaining2);
                    if actual_sleep.is_zero() {
                        continue;
                    }

                    tokio::select! {
                        _ = ctx.token.cancelled() => {
                            return Err(GroupCollectError {
                                _err: NGError::Error("Group collection cancelled".to_string()),
                                is_timeout: false,
                            });
                        }
                        _ = sleep(actual_sleep) => {}
                    }
                }
                RetryDecision::Exhausted => {
                    Self::record_collector_cycle(
                        &ctx.metrics_hub,
                        false,
                        is_timeout,
                        elapsed_ns,
                        elapsed_seconds,
                    );
                    return Err(GroupCollectError {
                        _err: err,
                        is_timeout,
                    });
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(name = "channel-collection", skip_all)]
    /// Channel data collection with simplified device handling
    async fn collect_channel_data(
        channel_id: i32,
        channel_name: &str,
        southward_manager: &Arc<NGSouthwardManager>,
        metrics_hub: &Arc<NGMetricsHub>,
        config: &Collector,
        semaphore: &Arc<ArcSwap<Semaphore>>,
        southward_data_bus: &Arc<SouthwardDataBus>,
        cancellation_token: &CancellationToken,
    ) -> NGResult<()> {
        // Check if cancelled before starting
        if cancellation_token.is_cancelled() {
            return Ok(());
        }

        // Skip collection if channel is not collectable
        if !southward_manager.is_channel_collectable(channel_id) {
            return Ok(());
        }

        let device_ids = southward_manager.get_collectable_device_ids(channel_id);
        if device_ids.is_empty() {
            return Ok(());
        }

        debug!(
            "📡 Channel [{channel_name}] processing {} devices",
            device_ids.len()
        );

        let start_time = Utc::now();

        // Build device entries and group calls (batching plan) up-front.
        let (driver, entries) = Self::build_device_entries(southward_manager, device_ids)?;
        if entries.is_empty() {
            debug!("📭 Channel [{channel_name}] no collectable points; skipping tick");
            return Ok(());
        }
        let driver = driver.ok_or(
            NGError::Error(format!(
                "Invariant violation: channel [{channel_name}] has {} collectable device entries but no driver handle",
                entries.len()
            ))
        )?;

        let groups = Self::build_group_calls(&driver, entries);
        if groups.is_empty() {
            return Ok(());
        }

        debug!(
            "📦 Channel [{channel_name}] built {} collection groups",
            groups.len(),
        );

        let prom = southward_manager.get_channel_metric_handles(channel_id);
        let total_budget = Duration::from_millis(config.collection_timeout_ms());
        let retry_policy = config.retry_policy();
        let max_in_flight = config.max_concurrent_collections().max(1);

        // Execute groups concurrently without spawning per-device tasks.
        //
        // Why: per-device `tokio::spawn` creates significant scheduling overhead at scale.
        // We keep concurrency via `buffer_unordered` + a shared semaphore (global bound).
        let mut successful = 0;
        let mut failed = 0;
        let mut timeouts = 0;

        let mut stream = futures::stream::iter(groups.into_iter())
            .map(|group| {
                let driver = Arc::clone(&driver);
                let ctx = GroupCollectContext {
                    total_budget,
                    retry_policy,
                    semaphore: Arc::clone(semaphore),
                    metrics_hub: Arc::clone(metrics_hub),
                    prom: prom.clone(),
                    token: cancellation_token.clone(),
                };
                async move { Self::collect_one_group(driver, group, ctx).await }
            })
            // Keep at most N futures in-flight per channel tick to limit memory and polling work.
            .buffer_unordered(max_in_flight);

        while let Some(result) = stream.next().await {
            match result {
                Ok(group_data) => {
                    successful += 1;
                    if !group_data.is_empty() {
                        Self::send_data(southward_data_bus, group_data).await;
                    }
                }
                Err(e) => {
                    if e.is_timeout {
                        timeouts += 1;
                    } else {
                        failed += 1;
                    }
                }
            }
        }

        // Keep approximate semaphore metrics updated (best-effort).
        let available = semaphore.load().available_permits() as u64;
        let total = config.max_concurrent_collections() as u64;
        let current = total.saturating_sub(available);
        metrics_hub.set_collector_concurrency_permits(current, available);

        let collection_duration_ms = (Utc::now() - start_time).num_milliseconds();
        debug!("📈 Channel [{channel_name}] collection completed: {successful} successful, {failed} failed, {timeouts} timeouts, duration in {collection_duration_ms}ms");

        Ok(())
    }

    /// Send data for improved throughput
    #[inline]
    async fn send_data(southward_data_bus: &Arc<SouthwardDataBus>, data: Vec<NorthwardData>) {
        let tx = southward_data_bus.sender();
        for item in data.into_iter() {
            if let Err(e) = tx.send(Arc::new(item)).await {
                error!(error=%e, "Failed to send item to northward system");
            }
        }
    }

    #[instrument(name = "collection-engine-stop", skip_all)]
    /// Stop the collection engine gracefully
    pub async fn stop(&self) -> NGResult<()> {
        info!("🛑 Stopping collection engine gracefully");

        // Cancel all collection tasks
        self.master_token.cancel();

        // Wait for all tasks to complete with timeout
        let task_handles: Vec<_> = {
            let mut tasks = self.collection_tasks.write().await;
            tasks.drain().map(|(_, handle)| handle).collect()
        };

        let shutdown_timeout = Duration::from_secs(30);
        let shutdown_tasks: Vec<_> = task_handles
            .into_iter()
            .map(|handle| tokio::spawn(handle))
            .collect();

        match timeout(shutdown_timeout, join_all(shutdown_tasks)).await {
            Ok(results) => {
                let mut successful = 0;
                let mut failed = 0;

                for result in results {
                    match result {
                        Ok(Ok(_)) => successful += 1,
                        _ => failed += 1,
                    }
                }

                info!(
                    "✅ Collection engine stopped: {successful} tasks completed, {failed} failed",
                );
            }
            Err(_) => {
                warn!("⚠️ Some collection tasks did not complete within timeout");
            }
        }

        // Clear channel tokens
        self.channel_tokens.write().await.clear();
        self.refresh_active_tasks_metric().await;

        Ok(())
    }

    #[inline]
    /// Get current engine metrics
    pub async fn get_snapshot(&self) -> CollectorMetricsSnapshot {
        self.metrics_hub.snapshot_collector()
    }

    #[inline]
    /// Add a new channel for collection
    pub async fn add_channel(&self, channel_id: i32) -> NGResult<()> {
        let channel_token = self.master_token.child_token();
        self.channel_tokens
            .write()
            .await
            .insert(channel_id, channel_token);
        self.start_channel_collection(channel_id).await?;
        self.refresh_active_tasks_metric().await;
        Ok(())
    }

    #[inline]
    /// Remove a channel from collection
    pub async fn remove_channel(&self, channel_id: i32) {
        // Cancel the channel token
        if let Some(token) = self.channel_tokens.write().await.remove(&channel_id) {
            token.cancel();
        }

        // Remove and abort the task
        if let Some(task) = self.collection_tasks.write().await.remove(&channel_id) {
            if !task.is_finished() {
                task.abort();
            }
        }
        self.refresh_active_tasks_metric().await;
    }

    /// Restart channel collection
    pub async fn restart_channel(&self, channel_id: i32) -> NGResult<()> {
        // Remove existing channel
        self.remove_channel(channel_id).await;

        // Add new channel
        self.add_channel(channel_id).await?;

        Ok(())
    }

    #[inline]
    /// Get semaphore metrics (available permits only)
    pub async fn get_semaphore_metrics(&self) -> (usize, usize) {
        let available = self.semaphore.load().available_permits();
        let total = self.semaphore_limit.load(Ordering::Relaxed);
        (total.saturating_sub(available), available)
    }
}
