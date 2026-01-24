use super::{
    codec::ModbusCodec,
    planner::{ModbusPlanner, ModbusPlannerConfig},
    supervisor::{ModbusObservability, SessionEntry, SessionSupervisor, SharedSession},
    types::{
        ModbusChannel, ModbusConnection, ModbusDevice, ModbusFunctionCode, ModbusParameter,
        ModbusPoint,
    },
};
use async_trait::async_trait;
use chrono::Utc;
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, CollectItem, CollectionGroupKey, DataPointType, DataType,
    DeviceBuffers, Driver, DriverError, DriverHealth, DriverResult, ExecuteOutcome, ExecuteResult,
    HealthStatus, NGValue, NorthwardData, PointValue, RuntimeAction, RuntimeDevice,
    RuntimeParameter, RuntimePoint, SouthwardConnectionState, SouthwardInitContext, ValueCodec,
    WriteOutcome, WriteResult,
};
use serde_json::json;
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration as StdDuration, Instant},
};
use tokio::{
    sync::{mpsc, watch, Mutex},
    time::{timeout, Duration as TokioDuration},
};
use tokio_modbus::{
    client::{Context, Reader, Writer},
    slave::{Slave, SlaveContext as _},
    ExceptionCode,
};
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

/// Production-grade Modbus driver implementation with batching and connection pooling
///
/// - Batches reads by function code and address range using max_gap/max_batch
/// - Maintains a connection pool (TCP: N contexts, RTU: 1 context)
/// - Enforces timeouts and retry/backoff
/// - Zero-copy friendly where applicable and minimal allocations on hot path
///
/// Internal state is managed via `Arc<RwLock<Option<...>>>` to support:
/// - `init()` with `&mut self` (direct field access during initialization)
/// - `stop()` with `&self` (RwLock-based access after initialization)
pub struct ModbusDriver {
    /// Driver configuration
    inner: Arc<ModbusChannel>,
    /// Shared session pool state (TCP: N contexts, RTU: 1 context).
    session: SharedSession,
    /// Deferred reconnect receiver, created during init and consumed once in start
    reconnect_rx: Mutex<Option<mpsc::Receiver<()>>>,
    /// Started flag to prevent duplicate start
    started: std::sync::atomic::AtomicBool,
    /// Driver-level cancel token
    cancel_token: CancellationToken,
    /// Connection state channel
    conn_tx: watch::Sender<SouthwardConnectionState>,
    conn_rx: watch::Receiver<SouthwardConnectionState>,
    /// Host-injected observability config, taken once in `start()` (avoid clone).
    observability: Mutex<Option<ModbusObservability>>,
    /// Metrics
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    last_avg_response_time_ms: AtomicU64,
}

impl ModbusDriver {
    /// Collection group key namespace for Modbus slave ID grouping.
    ///
    /// ASCII: "MODB"
    const KIND_MODBUS_SLAVE: u32 = 0x4D4F_4442;

    /// Return the effective pool size for this channel configuration.
    ///
    /// # Notes
    /// - For RTU, this always returns `1` to preserve single-flight semantics.
    /// - For TCP, we clamp the configured `tcpPoolSize` to a conservative upper bound.
    #[inline]
    fn effective_pool_size(&self) -> usize {
        match &self.inner.config.connection {
            ModbusConnection::Tcp { .. } => self.inner.config.tcp_pool_size.clamp(1, 8) as usize,
            ModbusConnection::Rtu { .. } => 1,
        }
    }

    /// Pick a Modbus context from the current session pool (round-robin).
    ///
    /// This helper is allocation-free and triggers a reconnect when no pool is available.
    #[inline]
    fn pick_ctx(&self) -> DriverResult<Arc<tokio::sync::Mutex<Context>>> {
        let pool = self.session.pool.load_full();
        let Some(pool) = pool else {
            self.session.healthy.store(false, Ordering::Release);
            let _ = self
                .session
                .last_error
                .lock()
                .map(|mut g| *g = Some("no session pool".to_string()));
            let _ = self.session.reconnect_tx.try_send(());
            return Err(DriverError::ServiceUnavailable);
        };
        pool.pick().ok_or_else(|| {
            self.session.healthy.store(false, Ordering::Release);
            let _ = self
                .session
                .last_error
                .lock()
                .map(|mut g| *g = Some("empty session pool".to_string()));
            let _ = self.session.reconnect_tx.try_send(());
            DriverError::ServiceUnavailable
        })
    }

    /// Run a Modbus operation with timeout, unified error handling, reconnection notify and metrics.
    /// The closure receives a mutable context and should return a future producing
    /// tokio_modbus::Result<Result<T, tokio_modbus::ExceptionCode>>.
    #[inline]
    async fn run_op<T, F, Fut>(
        &self,
        ctx: Arc<tokio::sync::Mutex<Context>>,
        op_timeout: u64,
        op_label: &'static str,
        op: F,
    ) -> DriverResult<T>
    where
        F: FnOnce(Arc<tokio::sync::Mutex<Context>>) -> Fut + Send + 'static,
        Fut:
            Future<Output = Result<Result<T, ExceptionCode>, tokio_modbus::Error>> + Send + 'static,
        T: Send + 'static,
    {
        // Clone session state to move into the spawned task
        let session_shared = self.session.clone();

        let (res, elapsed_ms) = tokio::spawn(async move {
            let start_ts = Instant::now();
            let duration = StdDuration::from_millis(op_timeout);
            let res: DriverResult<T> = match timeout(duration, op(Arc::clone(&ctx))).await {
                Ok(Ok(inner)) => match inner {
                    Ok(v) => Ok(v),
                    Err(code) => Err(DriverError::ExecutionError(format!(
                        "Modbus exception on {}: {:?}",
                        op_label, code
                    ))),
                },
                Ok(Err(e)) => {
                    let msg = e.to_string();
                    warn!(op = op_label, err = %msg, "Transport error, request reconnect");
                    session_shared.healthy.store(false, Ordering::Release);
                    let _ = session_shared
                        .last_error
                        .lock()
                        .map(|mut g| *g = Some(msg.clone()));
                    let _ = session_shared.reconnect_tx.try_send(());
                    Err(DriverError::ExecutionError(msg))
                }
                Err(_elapsed) => {
                    warn!(op = op_label, "Operation timeout, request reconnect");
                    session_shared.healthy.store(false, Ordering::Release);
                    let _ = session_shared
                        .last_error
                        .lock()
                        .map(|mut g| *g = Some("timeout".to_string()));
                    let _ = session_shared.reconnect_tx.try_send(());
                    Err(DriverError::Timeout(TokioDuration::from_millis(op_timeout)))
                }
            };
            let elapsed = start_ts.elapsed().as_millis() as u64;
            (res, elapsed)
        })
        .await
        .map_err(|e| DriverError::ExecutionError(e.to_string()))?;

        // Unified metrics
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        match &res {
            Ok(_) => {
                self.successful_requests.fetch_add(1, Ordering::Relaxed);
                let prev = self.last_avg_response_time_ms.load(Ordering::Acquire);
                let new_avg = if prev == 0 {
                    elapsed_ms
                } else {
                    (prev.saturating_mul(9) + elapsed_ms) / 10
                };
                self.last_avg_response_time_ms
                    .store(new_avg, Ordering::Release);
            }
            Err(_) => {
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
            }
        }

        res
    }

    pub fn with_context(ctx: SouthwardInitContext) -> DriverResult<Self> {
        let inner = ctx
            .runtime_channel
            .downcast_arc::<ModbusChannel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid ModbusChannel".to_string()))?;

        let (conn_tx, conn_rx) = watch::channel(SouthwardConnectionState::Disconnected);
        let (reconnect_tx, reconnect_rx) = mpsc::channel::<()>(1);
        let shared = Arc::new(SessionEntry::new_empty(reconnect_tx));

        let obs = ModbusObservability {
            channel_id: ctx.channel_id,
            driver: ctx.driver,
            meter: ctx.transport_meter,
            transport: ctx.transport_factory,
        };

        Ok(Self {
            inner,
            session: shared,
            reconnect_rx: Mutex::new(Some(reconnect_rx)),
            started: AtomicBool::new(false),
            cancel_token: CancellationToken::new(),
            conn_tx,
            conn_rx,
            observability: Mutex::new(Some(obs)),
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            last_avg_response_time_ms: AtomicU64::new(0),
        })
    }

    /// Collect for a Modbus "physical group" (same slave id) and return northward payloads
    /// grouped by **business device**.
    ///
    /// # Notes
    /// - This method enforces the Modbus single-flight semantics on the underlying `Context`.
    /// - The read planner merges points across *all* business devices in the group.
    async fn collect_group_with_slave(
        &self,
        slave_id: u8,
        items: &[CollectItem],
    ) -> DriverResult<Vec<NorthwardData>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        // Prepare per-device output buffers and build a merged point list.
        let mut buffers = HashMap::with_capacity(items.len());
        let mut modbus_points = Vec::new();

        for (dev, points_any) in items.iter() {
            let md = dev.downcast_ref::<ModbusDevice>().ok_or_else(|| {
                DriverError::ConfigurationError(
                    "RuntimeDevice is not ModbusDevice for ModbusDriver".to_string(),
                )
            })?;
            if md.slave_id != slave_id {
                return Err(DriverError::ConfigurationError(
                    "collect_data items contain mixed slaveId".to_string(),
                ));
            }

            buffers
                .entry(md.id)
                .or_insert_with(|| DeviceBuffers::new(md.device_name.clone()));

            for p in points_any.iter() {
                let Some(mp) = p.downcast_ref::<ModbusPoint>() else {
                    continue;
                };
                if !matches!(mp.access_mode(), AccessMode::Read | AccessMode::ReadWrite) {
                    continue;
                }
                modbus_points.push(mp);
            }
        }

        if modbus_points.is_empty() {
            return Ok(Vec::new());
        }

        // Clamp batch parameters by Modbus hard limits.
        let cfg = ModbusPlannerConfig {
            max_gap_registers: self.inner.config.max_gap_registers,
            max_batch_registers: self.inner.config.max_batch_registers.clamp(1, 125),
            max_gap_bits: self.inner.config.max_gap_bits,
            max_batch_bits: self.inner.config.max_batch_bits.clamp(1, 2000),
        };

        let batches = ModbusPlanner::plan_read_batches(cfg, &modbus_points);

        // Pick one context for the whole collection cycle.
        //
        // - TCP: spreads different groups across pool contexts (higher throughput).
        // - RTU: pool size is effectively 1, so semantics are unchanged.
        let ctx_arc = self.pick_ctx()?;

        let slave = Slave(slave_id);
        let timeout_ms = self.inner.connection_policy.read_timeout_ms.max(1);
        let ts = Utc::now();

        for batch in batches {
            let op_label = match batch.function {
                ModbusFunctionCode::ReadCoils => "ReadCoils",
                ModbusFunctionCode::ReadDiscreteInputs => "ReadDiscreteInputs",
                ModbusFunctionCode::ReadHoldingRegisters => "ReadHoldingRegisters",
                ModbusFunctionCode::ReadInputRegisters => "ReadInputRegisters",
                _ => "UnknownRead",
            };

            // Validate function (defensive)
            match batch.function {
                ModbusFunctionCode::ReadCoils
                | ModbusFunctionCode::ReadDiscreteInputs
                | ModbusFunctionCode::ReadHoldingRegisters
                | ModbusFunctionCode::ReadInputRegisters => {}
                other => {
                    return Err(DriverError::ExecutionError(format!(
                        "Unsupported function: {:?}",
                        other
                    )));
                }
            }

            let func = batch.function;
            let start = batch.start_addr;
            let qty = batch.quantity;
            let ctx = Arc::clone(&ctx_arc);

            let op_res = self
                .run_op(ctx, timeout_ms, op_label, move |ctx| {
                    Box::pin(async move {
                        let mut guard = ctx.lock().await;
                        guard.set_slave(slave);
                        match func {
                            ModbusFunctionCode::ReadCoils => guard
                                .read_coils(start, qty)
                                .await
                                .map(|r| r.map(NorthwardReadResult::Coils)),
                            ModbusFunctionCode::ReadDiscreteInputs => guard
                                .read_discrete_inputs(start, qty)
                                .await
                                .map(|r| r.map(NorthwardReadResult::Coils)),
                            ModbusFunctionCode::ReadHoldingRegisters => guard
                                .read_holding_registers(start, qty)
                                .await
                                .map(|r| r.map(NorthwardReadResult::Registers)),
                            ModbusFunctionCode::ReadInputRegisters => guard
                                .read_input_registers(start, qty)
                                .await
                                .map(|r| r.map(NorthwardReadResult::Registers)),
                            _ => unreachable!(),
                        }
                    })
                })
                .await;

            match op_res {
                Ok(result) => match result {
                    NorthwardReadResult::Coils(bits) => {
                        for p in &batch.points {
                            let offset = p.address.saturating_sub(batch.start_addr) as usize;
                            let val = bits.get(offset).copied().unwrap_or(false);
                            let Some(buf) = buffers.get_mut(&p.device_id) else {
                                continue;
                            };
                            match p.r#type() {
                                DataPointType::Telemetry => {
                                    let Some(value) = ValueCodec::coerce_bool_to_value(
                                        val,
                                        p.logical_data_type(),
                                        &p.transform,
                                    ) else {
                                        warn!(
                                            point_id = p.id,
                                            key = %p.key,
                                            expected = ?p.logical_data_type(),
                                            "Failed to coerce coil value to NGValue; dropped"
                                        );
                                        continue;
                                    };
                                    buf.push(
                                        DataPointType::Telemetry,
                                        PointValue {
                                            point_id: p.id,
                                            point_key: Arc::<str>::from(p.key.as_str()),
                                            value,
                                        },
                                    );
                                }
                                DataPointType::Attribute => {
                                    let Some(value) = ValueCodec::coerce_bool_to_value(
                                        val,
                                        p.logical_data_type(),
                                        &p.transform,
                                    ) else {
                                        warn!(
                                            point_id = p.id,
                                            key = %p.key,
                                            expected = ?p.logical_data_type(),
                                            "Failed to coerce coil value to NGValue; dropped"
                                        );
                                        continue;
                                    };
                                    buf.push(
                                        DataPointType::Attribute,
                                        PointValue {
                                            point_id: p.id,
                                            point_key: Arc::<str>::from(p.key.as_str()),
                                            value,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    NorthwardReadResult::Registers(words) => {
                        for p in &batch.points {
                            let offset = p.address.saturating_sub(batch.start_addr) as usize;
                            let qty = p.quantity.max(1) as usize;
                            if offset + qty > words.len() {
                                warn!(
                                    key = %p.key,
                                    needed = qty,
                                    have = words.len().saturating_sub(offset),
                                    "Insufficient words for point"
                                );
                                continue;
                            }
                            let Some(buf) = buffers.get_mut(&p.device_id) else {
                                continue;
                            };
                            let slice = &words[offset..offset + qty];
                            let wire_dt = p.wire_data_type();
                            let logical_dt = p.logical_data_type();
                            let value = match ModbusCodec::parse_register_value(
                                slice,
                                wire_dt,
                                logical_dt,
                                self.inner.config.byte_order,
                                self.inner.config.word_order,
                                &p.transform,
                            ) {
                                Ok(v) => v,
                                Err(e) => {
                                    warn!(
                                        point_id = p.id,
                                        key = %p.key,
                                        err = %e,
                                        "Parse register to NGValue failed; dropped"
                                    );
                                    continue;
                                }
                            };
                            match p.r#type() {
                                DataPointType::Telemetry => {
                                    buf.push(
                                        DataPointType::Telemetry,
                                        PointValue {
                                            point_id: p.id,
                                            point_key: Arc::<str>::from(p.key.as_str()),
                                            value,
                                        },
                                    );
                                }
                                DataPointType::Attribute => {
                                    buf.push(
                                        DataPointType::Attribute,
                                        PointValue {
                                            point_id: p.id,
                                            point_key: Arc::<str>::from(p.key.as_str()),
                                            value,
                                        },
                                    );
                                }
                            }
                        }
                    }
                },
                Err(e) => return Err(e),
            }
        }

        // Build stable, per-business-device outputs with a single group timestamp.
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
}

#[async_trait]
impl Driver for ModbusDriver {
    #[instrument(level = "info", skip_all)]
    async fn start(&self) -> DriverResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // already started; make idempotent
            return Ok(());
        }
        let cancel = self.cancel_token.child_token();
        let shared = Arc::clone(&self.session);
        // Take ownership of reconnect receiver once
        let mut rx_guard = self.reconnect_rx.lock().await;
        let reconnect_rx = rx_guard.take().ok_or(DriverError::ExecutionError(
            "reconnect receiver already consumed".into(),
        ))?;
        let supervisor = SessionSupervisor::new(shared, cancel, self.conn_tx.clone(), reconnect_rx);
        let inner = Arc::clone(&self.inner);
        let obs = {
            let mut g = self.observability.lock().await;
            g.take().ok_or(DriverError::ExecutionError(
                "observability context already consumed".into(),
            ))?
        };
        supervisor.run(inner, obs).await;
        Ok(())
    }

    #[instrument(level = "info", skip_all)]
    async fn stop(&self) -> DriverResult<()> {
        self.cancel_token.cancel();
        self.session.shutdown.store(true, Ordering::Release);
        if let Some(pool) = self.session.pool.swap(None) {
            pool.disconnect_all(StdDuration::from_secs(2)).await;
        }
        Ok(())
    }

    #[inline]
    fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        device
            .downcast_ref::<ModbusDevice>()
            .map(|d| CollectionGroupKey::from_u64(Self::KIND_MODBUS_SLAVE, d.slave_id as u64))
    }

    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        let (device0, _points0) = items.first().ok_or(DriverError::ValidationError(
            "collect_data called with empty items".to_string(),
        ))?;
        let md0 = device0.downcast_ref::<ModbusDevice>().ok_or_else(|| {
            DriverError::ConfigurationError(
                "RuntimeDevice is not ModbusDevice for ModbusDriver".to_string(),
            )
        })?;
        self.collect_group_with_slave(md0.slave_id, items).await
    }

    #[inline]
    fn collect_max_inflight(&self) -> usize {
        self.effective_pool_size()
    }

    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let device =
            device
                .downcast_ref::<ModbusDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not ModbusDevice".to_string(),
                ))?;

        let resolved = downcast_parameters::<ModbusParameter>(parameters)?;
        let plans = ModbusPlanner::plan_write_plans(
            &resolved,
            self.inner.config.byte_order,
            self.inner.config.word_order,
        )?;

        // Execute write plans sequentially using a single picked context to preserve ordering.
        let ctx_arc = self.pick_ctx()?;

        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);
        let slave = Slave(device.slave_id);

        for plan in plans.into_iter() {
            let function = plan.function;
            let address = plan.address;
            let coils = plan.coils;
            let registers = plan.registers;

            let result: DriverResult<()> = match function {
                ModbusFunctionCode::WriteSingleCoil => {
                    let bit = coils
                        .as_ref()
                        .and_then(|v| v.first())
                        .copied()
                        .unwrap_or(false);
                    let ctx = Arc::clone(&ctx_arc);
                    self.run_op(ctx, timeout_ms, "WriteSingleCoil", move |ctx| {
                        Box::pin(async move {
                            let mut guard = ctx.lock().await;
                            guard.set_slave(slave);
                            guard.write_single_coil(address, bit).await
                        })
                    })
                    .await
                }
                ModbusFunctionCode::WriteMultipleCoils => {
                    let coils_vec = coils.unwrap_or_default();
                    let ctx = Arc::clone(&ctx_arc);
                    self.run_op(ctx, timeout_ms, "WriteMultipleCoils", move |ctx| {
                        Box::pin(async move {
                            let mut guard = ctx.lock().await;
                            guard.set_slave(slave);
                            guard.write_multiple_coils(address, &coils_vec[..]).await
                        })
                    })
                    .await
                }
                ModbusFunctionCode::WriteSingleRegister => {
                    let reg = registers
                        .as_ref()
                        .and_then(|v| v.first())
                        .copied()
                        .unwrap_or(0);
                    let ctx = Arc::clone(&ctx_arc);
                    self.run_op(ctx, timeout_ms, "WriteSingleRegister", move |ctx| {
                        Box::pin(async move {
                            let mut guard = ctx.lock().await;
                            guard.set_slave(slave);
                            guard.write_single_register(address, reg).await
                        })
                    })
                    .await
                }
                ModbusFunctionCode::WriteMultipleRegisters => {
                    let regs_vec = registers.unwrap_or_default();
                    let ctx = Arc::clone(&ctx_arc);
                    self.run_op(ctx, timeout_ms, "WriteMultipleRegisters", move |ctx| {
                        Box::pin(async move {
                            let mut guard = ctx.lock().await;
                            guard.set_slave(slave);
                            guard.write_multiple_registers(address, &regs_vec[..]).await
                        })
                    })
                    .await
                }
                other => Err(DriverError::ConfigurationError(format!(
                    "Unsupported function in write phase: {:?}",
                    other
                ))),
            };

            result?;
        }

        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(json!(format!("Action '{}' executed", action.name()))),
        })
    }

    async fn write_point(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let device =
            device
                .downcast_ref::<ModbusDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not ModbusDevice for ModbusDriver".to_string(),
                ))?;
        let point = point
            .downcast_ref::<ModbusPoint>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not ModbusPoint for ModbusDriver".to_string(),
            ))?;

        if !matches!(point.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            return Err(DriverError::ValidationError(
                "point is not writeable".to_string(),
            ));
        }

        let effective_timeout_ms = timeout_ms
            .unwrap_or(self.inner.connection_policy.write_timeout_ms)
            .max(1);

        // Derive the write function code from the configured read function code (per design).
        let write_fc = match point.function_code {
            ModbusFunctionCode::ReadHoldingRegisters => {
                if point.quantity <= 1 {
                    ModbusFunctionCode::WriteSingleRegister
                } else {
                    ModbusFunctionCode::WriteMultipleRegisters
                }
            }
            ModbusFunctionCode::ReadCoils => {
                if point.quantity <= 1 {
                    ModbusFunctionCode::WriteSingleCoil
                } else {
                    ModbusFunctionCode::WriteMultipleCoils
                }
            }
            ModbusFunctionCode::ReadInputRegisters | ModbusFunctionCode::ReadDiscreteInputs => {
                return Err(DriverError::ConfigurationError(format!(
                    "Modbus function {:?} is read-only; write_point not supported",
                    point.function_code
                )));
            }
            // Defensive: if a write code is configured anyway, accept it.
            ModbusFunctionCode::WriteSingleCoil
            | ModbusFunctionCode::WriteMultipleCoils
            | ModbusFunctionCode::WriteSingleRegister
            | ModbusFunctionCode::WriteMultipleRegisters => point.function_code,
        };

        // Acquire a context snapshot from the session pool.
        let ctx_arc = self.pick_ctx()?;

        let slave = Slave(device.slave_id);
        let address = point.address;

        // Encode and execute exactly one write op.
        match write_fc {
            ModbusFunctionCode::WriteSingleCoil | ModbusFunctionCode::WriteMultipleCoils => {
                if point.wire_data_type() != DataType::Boolean {
                    return Err(DriverError::ValidationError(format!(
                        "coil write expects Boolean data_type, got {:?}",
                        point.wire_data_type()
                    )));
                }
                let target_len = Some(point.quantity.max(1) as usize);
                let coils = ModbusCodec::encode_coils(&value, target_len)?;

                match write_fc {
                    ModbusFunctionCode::WriteSingleCoil => {
                        let bit = coils.first().copied().unwrap_or(false);
                        let ctx = Arc::clone(&ctx_arc);
                        self.run_op(ctx, effective_timeout_ms, "WriteSingleCoil", move |ctx| {
                            Box::pin(async move {
                                let mut guard = ctx.lock().await;
                                guard.set_slave(slave);
                                guard.write_single_coil(address, bit).await
                            })
                        })
                        .await?;
                    }
                    ModbusFunctionCode::WriteMultipleCoils => {
                        let ctx = Arc::clone(&ctx_arc);
                        self.run_op(
                            ctx,
                            effective_timeout_ms,
                            "WriteMultipleCoils",
                            move |ctx| {
                                Box::pin(async move {
                                    let mut guard = ctx.lock().await;
                                    guard.set_slave(slave);
                                    guard.write_multiple_coils(address, &coils[..]).await
                                })
                            },
                        )
                        .await?;
                    }
                    _ => unreachable!(),
                }
            }
            ModbusFunctionCode::WriteSingleRegister
            | ModbusFunctionCode::WriteMultipleRegisters => {
                let mut regs = ModbusCodec::encode_registers_from_value(
                    &value,
                    point.wire_data_type(),
                    self.inner.config.byte_order,
                    self.inner.config.word_order,
                    point.quantity.max(1),
                )?;

                match write_fc {
                    ModbusFunctionCode::WriteSingleRegister => {
                        if regs.is_empty() {
                            return Err(DriverError::CodecError(
                                "encoded register payload is empty".to_string(),
                            ));
                        }
                        regs.truncate(1);
                        let reg = regs[0];
                        let ctx = Arc::clone(&ctx_arc);
                        self.run_op(
                            ctx,
                            effective_timeout_ms,
                            "WriteSingleRegister",
                            move |ctx| {
                                Box::pin(async move {
                                    let mut guard = ctx.lock().await;
                                    guard.set_slave(slave);
                                    guard.write_single_register(address, reg).await
                                })
                            },
                        )
                        .await?;
                    }
                    ModbusFunctionCode::WriteMultipleRegisters => {
                        let ctx = Arc::clone(&ctx_arc);
                        self.run_op(
                            ctx,
                            effective_timeout_ms,
                            "WriteMultipleRegisters",
                            move |ctx| {
                                Box::pin(async move {
                                    let mut guard = ctx.lock().await;
                                    guard.set_slave(slave);
                                    guard.write_multiple_registers(address, &regs[..]).await
                                })
                            },
                        )
                        .await?;
                    }
                    _ => unreachable!(),
                }
            }
            other => {
                return Err(DriverError::ConfigurationError(format!(
                    "Unsupported modbus write function: {:?}",
                    other
                )));
            }
        }

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value),
        })
    }

    #[inline]
    fn subscribe_connection_state(&self) -> watch::Receiver<SouthwardConnectionState> {
        self.conn_rx.clone()
    }

    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn health_check(&self) -> DriverResult<DriverHealth> {
        Ok(DriverHealth {
            status: if self.session.healthy.load(Ordering::Acquire) {
                HealthStatus::Healthy
            } else {
                HealthStatus::Unhealthy
            },
            last_activity: Utc::now(),
            error_count: self.failed_requests.load(Ordering::Acquire),
            success_rate: {
                let total = self.total_requests.load(Ordering::Acquire) as f64;
                if total > 0.0 {
                    self.successful_requests.load(Ordering::Acquire) as f64 / total
                } else {
                    0.0
                }
            },
            average_response_time: StdDuration::from_millis(
                self.last_avg_response_time_ms.load(Ordering::Acquire),
            ),
            details: None,
        })
    }
}

/// Internal enum for batch read results
enum NorthwardReadResult {
    Coils(Vec<bool>),
    Registers(Vec<u16>),
}
