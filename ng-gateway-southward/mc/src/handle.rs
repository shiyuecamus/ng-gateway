//! MC southward data-plane handle.
//!
//! This is the **only** hot-path object published by the SDK supervision loop.
//! It MUST be cheap to clone (`Arc`), safe to use concurrently, and avoid extra allocations.
//!
//! Design:
//! - The protocol session pool is attached/detached by `McSession`.
//! - Data-plane methods fail fast with `ServiceUnavailable` when no active session exists.
//! - MC's SLMP protocol is serial (no request correlation), so concurrency is achieved
//!   via a connection pool where each member handles one batch at a time.

use super::{
    codec::McCodec,
    protocol::{
        planner::{PlannerConfig, WriteEntry},
        session::Session as ProtoSession,
    },
    typed_api::{McTypedApi, TypedPointReadSpec},
    types::{McAction, McChannel, McDevice, McParameter, McPoint},
};
use arc_swap::ArcSwapOption;
use chrono::Utc;
// `join_all` used for partial-success semantics in pool-based concurrent reads.
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, CollectItem, CollectionGroupKey, CollectorConcurrencyProfile,
    DataType, DeviceBuffers, DriverError, DriverResult, ExecuteOutcome, ExecuteResult, NGValue,
    NorthwardData, PointValue, RuntimeAction, RuntimeDelta, RuntimeDevice, RuntimeParameter,
    RuntimePoint, SouthwardHandle, ValueCodec, WriteOutcome, WriteResult,
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
use tracing::{instrument, warn};

/// Connection pool for MC protocol sessions.
///
/// MC's SLMP protocol is strictly serial (send request → wait response → next),
/// so a single TCP connection can only process one request at a time. This pool
/// distributes batches across multiple independent connections using lock-free
/// round-robin, reducing end-to-end latency from `N_batches * RTT` to roughly
/// `ceil(N_batches / pool_size) * RTT`.
pub struct McSessionPool {
    /// Pool of independent protocol sessions, each backed by its own TCP connection
    /// and event loop task.
    sessions: Vec<Arc<ProtoSession>>,
    /// Lock-free round-robin counter for session selection.
    rr: AtomicUsize,
}

impl McSessionPool {
    /// Create a new pool from a list of active protocol sessions.
    pub fn new(sessions: Vec<Arc<ProtoSession>>) -> Self {
        Self {
            sessions,
            rr: AtomicUsize::new(0),
        }
    }

    /// Round-robin pick a session from the pool.
    #[inline]
    pub fn pick(&self) -> Option<Arc<ProtoSession>> {
        let n = self.sessions.len();
        if n == 0 {
            return None;
        }
        let i = self.rr.fetch_add(1, Ordering::Relaxed) % n;
        Some(Arc::clone(&self.sessions[i]))
    }

    /// Pick a session by explicit index (for deterministic batch distribution).
    #[inline]
    pub fn pick_by_index(&self, idx: usize) -> Option<Arc<ProtoSession>> {
        let n = self.sessions.len();
        if n == 0 {
            return None;
        }
        Some(Arc::clone(&self.sessions[idx % n]))
    }

    /// Number of sessions in the pool.
    #[inline]
    pub fn pool_size(&self) -> usize {
        self.sessions.len()
    }

    /// Shutdown all sessions in the pool.
    pub(crate) async fn shutdown_all(&self) {
        for session in &self.sessions {
            session.shutdown().await;
        }
    }
}

impl std::fmt::Debug for McSessionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McSessionPool")
            .field("pool_size", &self.sessions.len())
            .finish()
    }
}

/// MC data-plane handle published when the protocol session is Ready.
pub struct McHandle {
    /// Typed runtime channel configuration.
    inner: Arc<McChannel>,
    /// Current active protocol session pool (lock-free hot reads).
    pool: ArcSwapOption<McSessionPool>,
    /// Metrics: total collect/control operations (best-effort).
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    last_avg_response_time_ms: AtomicU64,
}

impl McHandle {
    /// Collection group key namespace for grouping by channel.
    ///
    /// ASCII: "MCCH"
    const KIND_MC_CHANNEL: u32 = 0x4D43_4348;

    /// Create a new handle for the given channel (no I/O).
    #[inline]
    pub fn new(inner: Arc<McChannel>) -> Self {
        Self {
            inner,
            pool: ArcSwapOption::from(None),
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            last_avg_response_time_ms: AtomicU64::new(0),
        }
    }

    /// Attach a protocol session pool for this attempt.
    ///
    /// This is called exactly once per attempt, after all sessions become Active.
    #[inline]
    pub(crate) fn attach_pool(&self, pool: Arc<McSessionPool>) {
        self.pool.store(Some(pool));
    }

    /// Detach protocol session pool (best-effort).
    ///
    /// Returns the previously attached pool (if any) so the caller can
    /// perform a graceful shutdown via [`McSessionPool::shutdown_all`].
    #[inline]
    pub(crate) fn detach_pool(&self) -> Option<Arc<McSessionPool>> {
        self.pool.swap(None)
    }

    #[inline]
    fn load_pool(&self) -> DriverResult<Arc<McSessionPool>> {
        self.pool.load_full().ok_or(DriverError::ServiceUnavailable)
    }

    /// Pick a single session from the pool for non-batched operations.
    #[inline]
    fn pick_session(&self) -> DriverResult<Arc<ProtoSession>> {
        self.load_pool()?
            .pick()
            .ok_or(DriverError::ServiceUnavailable)
    }

    /// Effective pool size derived from channel configuration.
    pub(crate) fn effective_pool_size(&self) -> usize {
        self.inner.config.concurrent_requests.unwrap_or(1).max(1) as usize
    }

    /// Compute word-length (MC "points") for a given data type.
    ///
    /// For string values, `string_len_bytes` controls how many bytes are read and is rounded up to
    /// whole words.
    fn words_for_data_type(
        data_type: DataType,
        string_len_bytes: Option<u16>,
    ) -> DriverResult<u16> {
        let words = match data_type {
            DataType::Boolean => 1,
            DataType::Int8 | DataType::UInt8 => 1,
            DataType::Int16 | DataType::UInt16 => 1,
            DataType::Int32 | DataType::UInt32 | DataType::Float32 => 2,
            DataType::Int64 | DataType::UInt64 | DataType::Float64 => 4,
            DataType::String => {
                let bytes = string_len_bytes.ok_or(DriverError::ConfigurationError(
                    "MC String point requires stringLenBytes configuration".to_string(),
                ))?;
                (bytes as u32).div_ceil(2) as u16
            }
            DataType::Binary | DataType::Timestamp => {
                return Err(DriverError::ConfigurationError(
                    "MC Binary/Timestamp data types are not supported yet".to_string(),
                ))
            }
        };
        Ok(words.max(1))
    }

    #[inline]
    fn update_metrics(&self, start_ts: Instant, success: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if success {
            let elapsed_ms = start_ts.elapsed().as_millis() as u64;
            self.successful_requests.fetch_add(1, Ordering::Relaxed);
            let prev = self.last_avg_response_time_ms.load(Ordering::Relaxed);
            let new_avg = if prev == 0 {
                elapsed_ms
            } else {
                (prev.saturating_mul(9) + elapsed_ms) / 10
            };
            self.last_avg_response_time_ms
                .store(new_avg, Ordering::Relaxed);
        } else {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[async_trait::async_trait]
impl SouthwardHandle for McHandle {
    fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        device
            .downcast_ref::<McDevice>()
            .map(|d| CollectionGroupKey::from_u64(Self::KIND_MC_CHANNEL, d.channel_id as u64))
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

        // Prepare per-device output buffers and build a merged point list.
        let mut buffers = HashMap::with_capacity(items.len());
        let mut mc_points = Vec::new();
        let mut point_device_ids = Vec::new();

        for (dev_any, points_any) in items.iter() {
            let dev = dev_any
                .downcast_ref::<McDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not McDevice for McHandle".into(),
                ))?;

            buffers
                .entry(dev.id)
                .or_insert_with(|| DeviceBuffers::new(dev.device_name.clone()));

            for p_any in points_any.iter() {
                let Ok(p) = Arc::clone(p_any).downcast_arc::<McPoint>() else {
                    continue;
                };
                if !matches!(p.access_mode, AccessMode::Read | AccessMode::ReadWrite) {
                    continue;
                }
                point_device_ids.push(dev.id);
                mc_points.push(p);
            }
        }

        if mc_points.is_empty() {
            return Ok(Vec::new());
        }

        let pool = self.load_pool()?;

        let mut specs: Vec<TypedPointReadSpec> = Vec::with_capacity(mc_points.len());
        for (index, point) in mc_points.iter().enumerate() {
            let addr = point
                .address
                .logical
                .clone()
                .ok_or(DriverError::ConfigurationError(format!(
                    "MC logical address not resolved for point '{}'",
                    point.key
                )))?;

            if addr.device.is_bit() {
                if point.wire_data_type() != DataType::Boolean {
                    return Err(DriverError::ConfigurationError(format!(
                        "MC bit device {:?} only supports Boolean data type for point '{}'",
                        addr.device, point.key
                    )));
                }
                if addr.bit.is_some() {
                    return Err(DriverError::ConfigurationError(format!(
                        "Bit-indexed MC address '{}' is not supported yet for collect_data",
                        point.address.raw
                    )));
                }
                let device_code =
                    addr.device
                        .device_code_3e()
                        .ok_or(DriverError::ConfigurationError(format!(
                            "Unsupported MC bit device type for batch read: {:?}",
                            addr.device
                        )))?;

                specs.push(TypedPointReadSpec {
                    index,
                    data_type: point.wire_data_type(),
                    addr,
                    word_len: 1,
                    device_code,
                });
            } else {
                if addr.bit.is_some() || (!addr.device.is_word() && !addr.device.is_dword()) {
                    return Err(DriverError::ConfigurationError(format!(
                        "Unsupported MC address for collect_data: '{}'",
                        point.address.raw
                    )));
                }
                if addr.device.is_forbidden_batch_word_read() {
                    return Err(DriverError::ConfigurationError(format!(
                        "MC device type {:?} is not allowed for batch word read (point '{}')",
                        addr.device, point.key
                    )));
                }

                let word_len =
                    Self::words_for_data_type(point.wire_data_type(), point.string_len_bytes)?;
                let device_code =
                    addr.device
                        .device_code_3e()
                        .ok_or(DriverError::ConfigurationError(format!(
                            "Unsupported MC device type for batch read: {:?}",
                            addr.device
                        )))?;

                specs.push(TypedPointReadSpec {
                    index,
                    data_type: point.wire_data_type(),
                    addr,
                    word_len,
                    device_code,
                });
            }
        }

        let series_max = self.inner.config.series.device_batch_in_word_points_max();
        let max_points = self
            .inner
            .config
            .max_points_per_batch
            .unwrap_or(series_max)
            .max(1);
        let max_bytes = self.inner.config.max_bytes_per_frame.unwrap_or(4096).max(1);
        let planner_cfg = PlannerConfig::new(max_points, max_bytes);

        let start_ts = Instant::now();

        // Distribute specs across pool members for concurrent execution.
        //
        // MC's SLMP protocol is strictly serial per connection, so concurrency
        // is achieved by distributing work across a pool of TCP connections.
        // Each pool member independently plans and executes its subset of specs
        // through the standard typed API.
        let pool_size = pool.pool_size();
        let read_results = if pool_size <= 1 {
            // Fast path: single connection, no distribution overhead.
            let session = pool.pick().ok_or(DriverError::ServiceUnavailable)?;
            McTypedApi::read_points_typed(&session, &planner_cfg, specs)
                .await
                .map_err(|e| DriverError::ExecutionError(e.to_string()))?
        } else {
            // Distribute specs across pool members grouped by device_code.
            //
            // Sort by the same key the planner uses for coalescing (device_code, head)
            // so that contiguous addresses stay in the same session. This preserves
            // the planner's ability to merge adjacent reads, preventing request count
            // inflation that a naive round-robin would cause.
            specs.sort_by_key(|s| (s.device_code, s.addr.head));

            let mut groups: Vec<Vec<TypedPointReadSpec>> =
                (0..pool_size).map(|_| Vec::new()).collect();
            let mut session_idx = 0usize;
            let mut prev_device_code: Option<u16> = None;

            for spec in specs.into_iter() {
                if prev_device_code != Some(spec.device_code) {
                    prev_device_code = Some(spec.device_code);
                    session_idx = (session_idx + 1) % pool_size;
                }
                groups[session_idx].push(spec);
            }

            let futs = groups.into_iter().enumerate().map(|(idx, group_specs)| {
                let session = pool.pick_by_index(idx);
                let cfg = planner_cfg;
                async move {
                    if group_specs.is_empty() {
                        return Ok(Vec::new());
                    }
                    let session = session.ok_or(DriverError::ServiceUnavailable)?;
                    McTypedApi::read_points_typed(&session, &cfg, group_specs)
                        .await
                        .map_err(|e| DriverError::ExecutionError(e.to_string()))
                }
            });

            let group_results = futures::future::join_all(futs).await;
            // Flatten successful group results; log failures but preserve partial data.
            let mut merged = Vec::new();
            let mut any_success = false;
            for (idx, r) in group_results.into_iter().enumerate() {
                match r {
                    Ok(items) => {
                        if !items.is_empty() {
                            any_success = true;
                        }
                        merged.extend(items);
                    }
                    Err(e) => {
                        tracing::warn!(pool_member = idx, error = %e, "MC pool member read failed; partial results preserved");
                    }
                }
            }
            if !any_success && pool_size > 0 {
                return Err(DriverError::ExecutionError(
                    "All MC pool members failed".to_string(),
                ));
            }
            merged
        };

        match &read_results.is_empty() {
            true => self.update_metrics(start_ts, false),
            false => self.update_metrics(start_ts, true),
        }

        for item in read_results.into_iter() {
            if item.end_code != 0 || item.index >= mc_points.len() {
                continue;
            }
            let point = &mc_points[item.index];
            let Some(wire_value) = item.value else {
                continue;
            };

            let wire_dt = point.wire_data_type();
            let logical_dt = point.logical_data_type();
            let value = match ValueCodec::wire_to_logical_value(
                &wire_value,
                wire_dt,
                logical_dt,
                &point.transform,
            ) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        point_id = point.id,
                        point_key = %point.key,
                        wire = ?wire_dt,
                        logical = ?logical_dt,
                        error = %e,
                        "MC uplink wire->logical conversion failed - dropped"
                    );
                    continue;
                }
            };

            let device_id = point_device_ids[item.index];
            let Some(buf) = buffers.get_mut(&device_id) else {
                continue;
            };
            buf.push(
                point.r#type,
                PointValue {
                    point_id: point.id,
                    point_key: Arc::<str>::from(point.key.as_str()),
                    value,
                },
            );
        }

        let ts = Utc::now();
        let mut device_ids: Vec<i32> = buffers.keys().copied().collect();
        device_ids.sort_unstable();

        let mut out: Vec<NorthwardData> = Vec::with_capacity(device_ids.len() * 2);
        for device_id in device_ids {
            let Some(buf) = buffers.remove(&device_id) else {
                continue;
            };
            out.extend(buf.into_northward(device_id, ts));
        }

        Ok(out)
    }

    #[instrument(level = "debug", skip_all)]
    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let action = action
            .downcast_ref::<McAction>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimeAction is not McAction".to_string(),
            ))?;
        let device = device
            .downcast_ref::<McDevice>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimeDevice is not McDevice".to_string(),
            ))?;

        let resolved = downcast_parameters::<McParameter>(parameters)?;

        let session = self.pick_session()?;

        let mut entries = Vec::with_capacity(resolved.len());

        for (param, value) in resolved.iter() {
            let addr = param
                .address
                .logical
                .clone()
                .ok_or(DriverError::ConfigurationError(format!(
                    "MC logical address not resolved for point '{}'",
                    param.key
                )))?;

            let (word_len, device_code) = if addr.device.is_bit() {
                // Bit devices currently only support Boolean data type for actions.
                if param.wire_data_type() != DataType::Boolean {
                    return Err(DriverError::ConfigurationError(format!(
                        "MC bit device {:?} only supports Boolean data type in action '{}'",
                        addr.device, action.name
                    )));
                }
                if addr.bit.is_some() {
                    return Err(DriverError::ConfigurationError(format!(
                        "Bit-indexed MC address '{}' is not supported yet for execute_action",
                        param.address.raw
                    )));
                }
                let device_code =
                    addr.device
                        .device_code_3e()
                        .ok_or(DriverError::ConfigurationError(format!(
                            "Unsupported MC bit device type for batch write: {:?}",
                            addr.device
                        )))?;
                (1, device_code)
            } else {
                if addr.bit.is_some() || (!addr.device.is_word() && !addr.device.is_dword()) {
                    return Err(DriverError::ConfigurationError(format!(
                        "Unsupported MC address for execute_action: '{}'",
                        param.address.raw
                    )));
                }

                // Enforce MC specification restrictions for batch word write.
                if addr.device.is_forbidden_batch_word_write() {
                    return Err(DriverError::ConfigurationError(format!(
                        "MC device type {:?} is not allowed for batch word write (action '{}')",
                        addr.device, action.name
                    )));
                }

                let word_len =
                    Self::words_for_data_type(param.wire_data_type(), param.string_len_bytes)?;
                let device_code =
                    addr.device
                        .device_code_3e()
                        .ok_or(DriverError::ConfigurationError(format!(
                            "Unsupported MC device type for batch write: {:?}",
                            addr.device
                        )))?;
                (word_len, device_code)
            };

            let data_bytes = McCodec::encode_typed(param.wire_data_type(), value)?;

            entries.push(WriteEntry {
                addr,
                word_len,
                device_code,
                data: bytes::Bytes::from(data_bytes),
            });
        }

        // Execute typed write via session; update metrics once per execute call.
        let series_max = self.inner.config.series.device_batch_in_word_points_max();
        let max_points = self
            .inner
            .config
            .max_points_per_batch
            .unwrap_or(series_max)
            .max(1);
        let max_bytes = self.inner.config.max_bytes_per_frame.unwrap_or(4096).max(1);

        let start_ts = Instant::now();
        if let Err(e) = McTypedApi::write_points_typed(
            &session,
            &PlannerConfig::new(max_points, max_bytes),
            entries,
        )
        .await
        {
            self.update_metrics(start_ts, false);
            return Err(DriverError::ExecutionError(e.to_string()));
        }
        self.update_metrics(start_ts, true);

        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(json!({
                "status": "ok",
                "deviceId": device.id,
                "action": action.name,
            })),
        })
    }

    #[instrument(level = "debug", skip_all)]
    async fn write_point(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let _device = device
            .downcast_ref::<McDevice>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimeDevice is not McDevice for McHandle".to_string(),
            ))?;
        let point = point
            .downcast_ref::<McPoint>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not McPoint for McHandle".to_string(),
            ))?;

        if !matches!(point.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            return Err(DriverError::ValidationError(
                "point is not writeable".to_string(),
            ));
        }

        let effective_timeout_ms = timeout_ms
            .unwrap_or(self.inner.connection_policy.write_timeout_ms)
            .max(1);
        let timeout_duration = tokio::time::Duration::from_millis(effective_timeout_ms);

        let addr = point
            .address
            .logical
            .clone()
            .ok_or(DriverError::ConfigurationError(format!(
                "MC logical address not resolved for point '{}'",
                point.key
            )))?;

        let (word_len, device_code) = if addr.device.is_bit() {
            if point.wire_data_type() != DataType::Boolean {
                return Err(DriverError::ConfigurationError(format!(
                    "MC bit device {:?} only supports Boolean data type for write_point",
                    addr.device
                )));
            }
            if addr.bit.is_some() {
                return Err(DriverError::ConfigurationError(format!(
                    "Bit-indexed MC address '{}' is not supported yet for write_point",
                    point.address.raw
                )));
            }
            let device_code =
                addr.device
                    .device_code_3e()
                    .ok_or(DriverError::ConfigurationError(format!(
                        "Unsupported MC bit device type for write_point: {:?}",
                        addr.device
                    )))?;
            (1, device_code)
        } else {
            if addr.bit.is_some() || (!addr.device.is_word() && !addr.device.is_dword()) {
                return Err(DriverError::ConfigurationError(format!(
                    "Unsupported MC address for write_point: '{}'",
                    point.address.raw
                )));
            }
            if addr.device.is_forbidden_batch_word_write() {
                return Err(DriverError::ConfigurationError(format!(
                    "MC device type {:?} is not allowed for batch word write (write_point)",
                    addr.device
                )));
            }
            let word_len =
                Self::words_for_data_type(point.wire_data_type(), point.string_len_bytes)?;
            let device_code =
                addr.device
                    .device_code_3e()
                    .ok_or(DriverError::ConfigurationError(format!(
                        "Unsupported MC device type for write_point: {:?}",
                        addr.device
                    )))?;
            (word_len, device_code)
        };

        let dt = value.data_type();
        let data_bytes: Vec<u8> =
            value
                .try_into()
                .map_err(|e: ng_gateway_sdk::NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected numeric value, got {:?}: {e}",
                        dt
                    ))
                })?;

        let session = self.pick_session()?;

        let series_max = self.inner.config.series.device_batch_in_word_points_max();
        let max_points = self
            .inner
            .config
            .max_points_per_batch
            .unwrap_or(series_max)
            .max(1);
        let max_bytes = self.inner.config.max_bytes_per_frame.unwrap_or(4096).max(1);

        let entries = vec![WriteEntry {
            addr,
            word_len,
            device_code,
            data: bytes::Bytes::from(data_bytes),
        }];
        let planner_cfg = PlannerConfig::new(max_points, max_bytes);

        let start_ts = Instant::now();
        let write_res = if timeout_ms.is_some() {
            match tokio::time::timeout(timeout_duration, {
                let session = Arc::clone(&session);
                async move { McTypedApi::write_points_typed(&session, &planner_cfg, entries).await }
            })
            .await
            {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(DriverError::ExecutionError(e.to_string())),
                Err(_elapsed) => Err(DriverError::Timeout(timeout_duration)),
            }
        } else {
            McTypedApi::write_points_typed(&session, &planner_cfg, entries)
                .await
                .map_err(|e| DriverError::ExecutionError(e.to_string()))
        };

        match &write_res {
            Ok(_) => self.update_metrics(start_ts, true),
            Err(_) => self.update_metrics(start_ts, false),
        }

        write_res?;

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value.clone()),
        })
    }

    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        // TODO(delta): Implement when MC needs dynamic runtime model updates.
        Ok(())
    }
}
