//! Modbus southward data-plane handle.
//!
//! This is the **only** hot-path object published by the SDK supervision loop.
//! It performs batching/planning and executes Modbus operations on a connected context pool.

use super::{
    codec::ModbusCodec,
    planner::{ModbusPlanner, ModbusPlannerConfig},
    types::{
        ModbusChannel, ModbusConnection, ModbusDevice, ModbusFunctionCode, ModbusParameter,
        ModbusPoint,
    },
};
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use chrono::Utc;
use futures_util::future::join_all;
use ng_gateway_sdk::{
    downcast_parameters, supervision::ReconnectHandle, AccessMode, CollectItem, CollectionGroupKey,
    CollectorConcurrencyProfile, DataType, DeviceBuffers, DriverError, DriverResult,
    ExecuteOutcome, ExecuteResult, NGValue, NorthwardData, PointValue, RuntimeAction, RuntimeDelta,
    RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardHandle, ValueCodec, WriteOutcome,
    WriteResult,
};
use serde_json::json;
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicU32, AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    time::Duration as StdDuration,
};
use tokio::{sync::Mutex, time::timeout};
use tokio_modbus::{
    client::{Client as _, Context, Reader, Writer},
    slave::{Slave, SlaveContext as _},
    ExceptionCode,
};
use tracing::warn;

/// A pool of Modbus contexts (each is single-flight via `Mutex<Context>`).
pub struct SessionPool {
    contexts: Vec<Arc<Mutex<Context>>>,
    rr: AtomicUsize,
}

impl SessionPool {
    pub fn new(contexts: Vec<Context>) -> Self {
        Self {
            contexts: contexts
                .into_iter()
                .map(|c| Arc::new(Mutex::new(c)))
                .collect(),
            rr: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn pick(&self) -> Option<Arc<Mutex<Context>>> {
        let n = self.contexts.len();
        if n == 0 {
            return None;
        }
        let i = self.rr.fetch_add(1, Ordering::Relaxed) % n;
        Some(Arc::clone(&self.contexts[i]))
    }

    pub async fn disconnect_all(self: Arc<Self>, timeout_each: StdDuration) {
        for ctx in &self.contexts {
            let ctx = Arc::clone(ctx);
            let _ = timeout(timeout_each, async move {
                let mut g = ctx.lock().await;
                g.disconnect().await
            })
            .await;
        }
    }
}

/// Modbus data-plane handle.
pub struct ModbusHandle {
    inner: Arc<ModbusChannel>,
    pool: ArcSwapOption<SessionPool>,
    reconnect: OnceLock<ReconnectHandle>,
    /// Consecutive timeout counter. Reconnect is requested only after
    /// exceeding `config.max_timeouts`. Transport errors bypass the counter.
    consecutive_timeouts: AtomicU32,
}

impl ModbusHandle {
    /// ASCII: "MODB"
    const KIND_MODBUS_SLAVE: u32 = 0x4D4F_4442;

    #[inline]
    pub fn new(inner: Arc<ModbusChannel>) -> Self {
        Self {
            inner,
            pool: ArcSwapOption::from(None),
            reconnect: OnceLock::new(),
            consecutive_timeouts: AtomicU32::new(0),
        }
    }

    #[inline]
    pub(crate) fn attach_pool(&self, pool: Arc<SessionPool>) {
        self.pool.store(Some(pool));
    }

    #[inline]
    pub(crate) fn detach_pool(&self) -> Option<Arc<SessionPool>> {
        self.pool.swap(None)
    }

    #[inline]
    pub(crate) fn set_reconnect(&self, reconnect: ReconnectHandle) {
        let _ = self.reconnect.set(reconnect);
    }

    #[inline]
    fn try_request_reconnect(&self, reason: &'static str) {
        if let Some(h) = self.reconnect.get() {
            let _ = h.try_request_reconnect(reason);
        }
    }

    #[inline]
    fn effective_pool_size(&self) -> usize {
        let size = match &self.inner.config.connection {
            ModbusConnection::Tcp { .. } => self.inner.config.tcp_pool_size.clamp(1, 32) as usize,
            ModbusConnection::Rtu { .. } => 1,
        };
        tracing::info!(
            channel_id = self.inner.id,
            tcp_pool_size = self.inner.config.tcp_pool_size,
            effective_size = size,
            "Modbus effective pool size calculated"
        );
        size
    }

    #[inline]
    fn pick_ctx(&self) -> DriverResult<Arc<Mutex<Context>>> {
        let pool = self.pool.load_full();
        let Some(pool) = pool else {
            self.try_request_reconnect("modbus no session pool");
            return Err(DriverError::ServiceUnavailable);
        };
        pool.pick().ok_or_else(|| {
            self.try_request_reconnect("modbus empty session pool");
            DriverError::ServiceUnavailable
        })
    }

    #[inline]
    async fn run_op<T, F, Fut>(
        &self,
        ctx: Arc<Mutex<Context>>,
        op_timeout_ms: u64,
        op_label: &'static str,
        op: F,
    ) -> DriverResult<T>
    where
        F: FnOnce(Arc<Mutex<Context>>) -> Fut + Send + 'static,
        Fut:
            Future<Output = Result<Result<T, ExceptionCode>, tokio_modbus::Error>> + Send + 'static,
        T: Send + 'static,
    {
        let duration = StdDuration::from_millis(op_timeout_ms.max(1));
        match timeout(duration, op(Arc::clone(&ctx))).await {
            Ok(Ok(inner)) => {
                // Success — reset consecutive timeout counter.
                self.consecutive_timeouts.store(0, Ordering::Relaxed);
                inner.map_err(|code| {
                    DriverError::ExecutionError(format!("Modbus exception on {op_label}: {code:?}"))
                })
            }
            Ok(Err(e)) => {
                // Transport error — always reconnect immediately (broken pipe, etc.).
                let msg = e.to_string();
                warn!(op = op_label, err = %msg, "Transport error, request reconnect");
                self.consecutive_timeouts.store(0, Ordering::Relaxed);
                self.try_request_reconnect("modbus transport error");
                Err(DriverError::ExecutionError(msg))
            }
            Err(_) => {
                // Timeout — use consecutive counter to avoid churn from transient slowness.
                let count = self.consecutive_timeouts.fetch_add(1, Ordering::Relaxed) + 1;
                let threshold = self.inner.config.max_timeouts.max(1);
                if count >= threshold {
                    warn!(
                        op = op_label,
                        consecutive_timeouts = count,
                        threshold,
                        "Timeout threshold reached, request reconnect"
                    );
                    self.consecutive_timeouts.store(0, Ordering::Relaxed);
                    self.try_request_reconnect("modbus timeout threshold reached");
                } else {
                    warn!(
                        op = op_label,
                        consecutive_timeouts = count,
                        threshold,
                        "Operation timeout (below reconnect threshold)"
                    );
                }
                Err(DriverError::Timeout(tokio::time::Duration::from_millis(
                    op_timeout_ms.max(1),
                )))
            }
        }
    }

    async fn collect_group_with_slave(
        &self,
        slave_id: u8,
        items: &[CollectItem],
    ) -> DriverResult<Vec<NorthwardData>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        let mut buffers = HashMap::with_capacity(items.len());
        let mut modbus_points = Vec::new();

        for (dev, points_any) in items.iter() {
            let md = dev
                .downcast_ref::<ModbusDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not ModbusDevice for ModbusHandle".to_string(),
                ))?;
            if md.slave_id != slave_id {
                return Err(DriverError::ConfigurationError(format!(
                    "collect_data items contain mixed slaveId: expected={slave_id}, got={} (device_id={}, device_name={})",
                    md.slave_id, md.id, md.device_name
                )));
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

        let cfg = ModbusPlannerConfig {
            max_gap_registers: self.inner.config.max_gap_registers,
            max_batch_registers: self.inner.config.max_batch_registers.clamp(1, 125),
            max_gap_bits: self.inner.config.max_gap_bits,
            max_batch_bits: self.inner.config.max_batch_bits.clamp(1, 2000),
        };
        let batches = ModbusPlanner::plan_read_batches(cfg, &modbus_points);
        let batches_len = batches.len();

        let slave = Slave(slave_id);
        let timeout_ms = self.inner.connection_policy.read_timeout_ms.max(1);
        let ts = Utc::now();

        // Execute read batches concurrently across the TCP context pool.
        //
        // Why: each slave may require multiple (e.g. 17) Modbus reads. Running these
        // sequentially on a single TCP session amplifies per-request latency and can
        // easily exceed a 1s collection period. By distributing batches across a pool,
        // we reduce end-to-end latency to roughly the max of a few request "waves".
        //
        // Safety: each `Context` is guarded by `Mutex` (single-flight). Concurrency is
        // achieved by using multiple contexts (connections) from the pool.
        let batch_parallelism = self.effective_pool_size().clamp(1, 8);
        let mut results: Vec<(usize, NorthwardReadResult)> = Vec::with_capacity(batches_len);
        let mut i = 0usize;
        while i < batches.len() {
            let end = (i + batch_parallelism).min(batches.len());
            let batches_ref = &batches;
            let futs = (i..end).map(|idx| async move {
                let batch = &batches_ref[idx];
                let op_label = match batch.function {
                    ModbusFunctionCode::ReadCoils => "ReadCoils",
                    ModbusFunctionCode::ReadDiscreteInputs => "ReadDiscreteInputs",
                    ModbusFunctionCode::ReadHoldingRegisters => "ReadHoldingRegisters",
                    ModbusFunctionCode::ReadInputRegisters => "ReadInputRegisters",
                    _ => "UnknownRead",
                };

                let func = batch.function;
                let start = batch.start_addr;
                let qty = batch.quantity;
                let ctx = self.pick_ctx()?;

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
                    .await?;

                Ok::<(usize, NorthwardReadResult), DriverError>((idx, op_res))
            });
            // Use `join_all` (not `try_join_all`) to preserve partial results.
            // Failed batches are logged and skipped; their points will have no
            // value in this cycle. Only when ALL batches across ALL waves fail
            // do we propagate an error.
            let chunk = join_all(futs).await;
            for r in chunk {
                match r {
                    Ok(item) => results.push(item),
                    Err(e) => {
                        tracing::warn!(error = %e, "Modbus read batch failed; skipping batch");
                    }
                }
            }
            i = end;
        }

        if results.is_empty() && !batches.is_empty() {
            return Err(DriverError::ExecutionError(
                "All Modbus read batches failed".to_string(),
            ));
        }

        for (idx, op_res) in results {
            let batch = &batches[idx];
            match op_res {
                NorthwardReadResult::Coils(bits) => {
                    for p in &batch.points {
                        let offset = p.address.saturating_sub(batch.start_addr) as usize;
                        let val = bits.get(offset).copied().unwrap_or(false);
                        let Some(buf) = buffers.get_mut(&p.device_id) else {
                            continue;
                        };
                        let Some(value) = ValueCodec::coerce_bool_to_value(
                            val,
                            p.logical_data_type(),
                            &p.transform,
                        ) else {
                            continue;
                        };
                        buf.push(
                            p.r#type(),
                            PointValue {
                                point_id: p.id,
                                point_key: Arc::<str>::from(p.key.as_str()),
                                value,
                            },
                        );
                    }
                }
                NorthwardReadResult::Registers(words) => {
                    for p in &batch.points {
                        let offset = p.address.saturating_sub(batch.start_addr) as usize;
                        let qty = p.quantity.max(1) as usize;
                        if offset + qty > words.len() {
                            continue;
                        }
                        let Some(buf) = buffers.get_mut(&p.device_id) else {
                            continue;
                        };
                        let slice = &words[offset..offset + qty];
                        let value = ModbusCodec::parse_register_value(
                            slice,
                            p.wire_data_type(),
                            p.logical_data_type(),
                            self.inner.config.byte_order,
                            self.inner.config.word_order,
                            &p.transform,
                        )?;
                        buf.push(
                            p.r#type(),
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
impl SouthwardHandle for ModbusHandle {
    #[inline]
    fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        device
            .downcast_ref::<ModbusDevice>()
            .map(|d| CollectionGroupKey::from_u64(Self::KIND_MODBUS_SLAVE, d.slave_id as u64))
    }

    #[inline]
    fn collector_concurrency_profile(&self) -> CollectorConcurrencyProfile {
        CollectorConcurrencyProfile::concurrent(self.effective_pool_size())
    }

    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        let (device, _) = items.first().ok_or(DriverError::ValidationError(
            "collect_data called with empty items".to_string(),
        ))?;
        let device =
            device
                .downcast_ref::<ModbusDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not ModbusDevice for ModbusHandle".to_string(),
                ))?;
        self.collect_group_with_slave(device.slave_id, items).await
    }

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
                    "Unsupported function in write phase: {other:?}"
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
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let device =
            device
                .downcast_ref::<ModbusDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not ModbusDevice".to_string(),
                ))?;
        let point = point
            .downcast_ref::<ModbusPoint>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not ModbusPoint for ModbusHandle".to_string(),
            ))?;

        if !matches!(point.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            return Err(DriverError::ValidationError(
                "point is not writeable".to_string(),
            ));
        }

        let effective_timeout_ms = timeout_ms
            .unwrap_or(self.inner.connection_policy.write_timeout_ms)
            .max(1);

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
            ModbusFunctionCode::WriteSingleCoil
            | ModbusFunctionCode::WriteMultipleCoils
            | ModbusFunctionCode::WriteSingleRegister
            | ModbusFunctionCode::WriteMultipleRegisters => point.function_code,
        };

        let ctx_arc = self.pick_ctx()?;
        let slave = Slave(device.slave_id);
        let address = point.address;

        match write_fc {
            ModbusFunctionCode::WriteSingleCoil | ModbusFunctionCode::WriteMultipleCoils => {
                if point.wire_data_type() != DataType::Boolean {
                    return Err(DriverError::ValidationError(format!(
                        "coil write expects Boolean data_type, got {:?}",
                        point.wire_data_type()
                    )));
                }
                let target_len = Some(point.quantity.max(1) as usize);
                let coils = ModbusCodec::encode_coils(value, target_len)?;
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
                    value,
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
                    "Unsupported modbus write function: {other:?}"
                )));
            }
        }

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value.clone()),
        })
    }

    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        Ok(())
    }
}

enum NorthwardReadResult {
    Coils(Vec<bool>),
    Registers(Vec<u16>),
}
