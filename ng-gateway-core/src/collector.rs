use crate::NGSouthwardManager;

use chrono::Utc;
use futures::{future::join_all, StreamExt};
use ng_gateway_common::metrics::{channel::InstrumentedSender, NGMetricsHub};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{core::metrics::CollectorMetricsSnapshot, settings::CollectorConfig};
use ng_gateway_sdk::{
    CollectItem, CollectionGroupKey, CollectionType, NorthwardData, RuntimeDevice, RuntimePoint,
};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{RwLock, Semaphore},
    task::JoinHandle,
    time::{interval, sleep, timeout, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

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

/// A single driver group call containing a batch of collect items.
///
/// Each group should share the same physical session semantics (e.g. Modbus slave ID)
/// so the southward driver can execute the batch efficiently.
#[derive(Clone)]
struct GroupCall {
    items: Vec<CollectItem>,
}

/// Collection engine with enhanced performance and reliability
#[derive(Clone)]
pub struct Collector {
    /// Engine configuration
    config: CollectorConfig,
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
    semaphore: Arc<Semaphore>,
    /// Data batch sender (bounded)
    data_tx: Arc<InstrumentedSender<Arc<NorthwardData>>>,
}

impl Collector {
    #[inline]
    /// Create a new collection engine
    pub fn new(
        config: CollectorConfig,
        southward_manager: Arc<NGSouthwardManager>,
        metrics_hub: Arc<NGMetricsHub>,
        data_tx: InstrumentedSender<Arc<NorthwardData>>,
        master_token: CancellationToken,
    ) -> Self {
        Self {
            config,
            southward_manager,
            metrics_hub,
            collection_tasks: Arc::new(RwLock::new(HashMap::new())),
            master_token,
            channel_tokens: Arc::new(RwLock::new(HashMap::new())),
            semaphore: Arc::new(Semaphore::new(config.max_concurrent_collections)),
            data_tx: Arc::new(data_tx),
        }
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

        // Update metrics
        self.metrics_hub
            .set_collector_active_tasks(successful as u64);

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

        if channel.config.collection_type() != CollectionType::Collection {
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
        let config = self.config;
        let semaphore = Arc::clone(&self.semaphore);
        let data_tx = Arc::clone(&self.data_tx);
        let channel_name = channel.config.name().to_string();

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
                            config,
                            &semaphore,
                            &data_tx,
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
        });

        self.collection_tasks.write().await.insert(channel_id, task);

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(name = "channel-collection", skip_all)]
    /// Channel data collection with simplified device handling
    async fn collect_channel_data(
        channel_id: i32,
        channel_name: &str,
        southward_manager: &Arc<NGSouthwardManager>,
        metrics_hub: &Arc<NGMetricsHub>,
        config: CollectorConfig,
        semaphore: &Arc<Semaphore>,
        data_tx: &Arc<InstrumentedSender<Arc<NorthwardData>>>,
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

        // In this gateway architecture, a channel tick should never mix multiple driver
        // instances: devices within the channel share the same `Arc<dyn Driver>`.
        //
        // Keep this as a debug assertion to detect unexpected topology bugs without
        // paying HashMap overhead on the hot path.
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

        if entries.is_empty() {
            return Ok(());
        }

        let mut groups: Vec<GroupCall> = Vec::new();
        let driver = match driver_handle {
            Some(d) => d,
            None => return Ok(()),
        };

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

        debug!(
            "📦 Channel [{channel_name}] built {} collection groups",
            groups.len(),
        );
        let driver = Arc::new(driver);
        let prom = southward_manager.get_channel_metric_handles(channel_id);

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
                let token = cancellation_token.clone();
                let prom = prom.clone();
                async move {
                    // Make semaphore acquire cancellable (owned permit)
                    let acquired = tokio::select! {
                        _ = token.cancelled() => {
                            return Err(NGError::Error("Group collection cancelled".to_string()));
                        }
                        p = Arc::clone(semaphore).acquire_owned() => p.map_err(|_| NGError::Error("Semaphore closed".to_string())),
                    }?;

                    let _permit = acquired;

                    // Per-group timeout (NOT per-device) to preserve batching semantics.
                    tokio::select! {
                        _ = token.cancelled() => {
                            Err(NGError::Error("Group collection cancelled".to_string()))
                        }
                        result = async {
                            let group_timeout = Duration::from_millis(config.collection_timeout_ms);
                            let io_start = Instant::now();
                            let (res, is_timeout) = match timeout(group_timeout, driver.collect_data(&group.items)).await {
                                Ok(inner) => (inner.map_err(|e| NGError::DriverError(e.to_string())), false),
                                Err(_) => (Err(NGError::Error("Group collection timeout".to_string())), true),
                            };

                            let elapsed = io_start.elapsed();
                            let elapsed_ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
                            let elapsed_seconds = elapsed.as_secs_f64();
                            let points_read = group
                                .items
                                .iter()
                                .map(|(_dev, pts)| pts.len() as u64)
                                .sum::<u64>();

                            // Southward per-channel I/O metrics (success/fail).
                            if let Some(h) = &prom {
                                if res.is_ok() {
                                    h.record_io_success(elapsed_ns, elapsed_seconds);
                                    h.record_collect_success(points_read, elapsed_seconds);
                                } else {
                                    h.record_io_failed(elapsed_ns, elapsed_seconds);
                                    if is_timeout {
                                        h.record_collect_timeout(points_read, elapsed_seconds);
                                    } else {
                                        h.record_collect_fail(points_read, elapsed_seconds);
                                    }
                                }
                            }

                            // Collector cycle metrics (success/fail/timeout).
                            if res.is_ok() {
                                metrics_hub.record_collector_cycle_success(elapsed_ns, elapsed_seconds);
                            } else if is_timeout {
                                metrics_hub.record_collector_cycle_timeout(elapsed_ns, elapsed_seconds);
                            } else {
                                metrics_hub.record_collector_cycle_fail(elapsed_ns, elapsed_seconds);
                            }

                            res
                        } => result,
                    }
                }
            })
            // Keep at most N futures in-flight per channel tick to limit memory and polling work.
            .buffer_unordered(config.max_concurrent_collections.max(1));

        while let Some(result) = stream.next().await {
            match result {
                Ok(group_data) => {
                    successful += 1;
                    if !group_data.is_empty() {
                        Self::send_data(data_tx, group_data).await;
                    }
                }
                Err(e) => {
                    let msg = e.to_string().to_lowercase();
                    if msg.contains("timeout") {
                        timeouts += 1;
                    } else {
                        failed += 1;
                    }
                }
            }
        }

        // Keep approximate semaphore metrics updated (best-effort).
        let available = semaphore.available_permits() as u64;
        metrics_hub.set_collector_concurrency_permits(available, available);

        let collection_duration_ms = (Utc::now() - start_time).num_milliseconds();
        debug!("📈 Channel [{channel_name}] collection completed: {successful} successful, {failed} failed, {timeouts} timeouts, duration in {collection_duration_ms}ms");

        Ok(())
    }

    /// Send data for improved throughput
    #[inline]
    async fn send_data(
        data_tx: &Arc<InstrumentedSender<Arc<NorthwardData>>>,
        data: Vec<NorthwardData>,
    ) {
        for item in data.into_iter() {
            if let Err(e) = data_tx.send(Arc::new(item)).await {
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
        let available = self.semaphore.available_permits();
        (available, available)
    }
}
