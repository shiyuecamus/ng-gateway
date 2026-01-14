use super::{
    codec::ModbusCodec,
    planner::{ModbusPlanner, ModbusPlannerConfig},
    supervisor::{SessionEntry, SessionSupervisor, SharedSession},
    types::{ModbusDevice, ModbusFunctionCode, ModbusParameter, ModbusPoint},
};
use crate::types::ModbusChannel;
use async_trait::async_trait;
use chrono::Utc;
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, AttributeData, DataPointType, DataType, Driver, DriverError,
    DriverHealth, DriverResult, ExecuteOutcome, ExecuteResult, HealthStatus, NGValue,
    NorthwardData, PointValue, RuntimeAction, RuntimeDevice, RuntimeParameter, RuntimePoint,
    SouthwardConnectionState, SouthwardInitContext, TelemetryData, ValueCodec, WriteOutcome,
    WriteResult,
};
use serde_json::json;
use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration as StdDuration, Instant},
};
use tokio::{
    sync::{mpsc, watch, Mutex},
    time::{timeout, Duration as TokioDuration},
};
use tokio_modbus::{
    client::{Client as _, Context, Reader, Writer},
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
    /// Shared single session
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
    /// Metrics
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    last_avg_response_time_ms: AtomicU64,
}

impl ModbusDriver {
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

        Ok(Self {
            inner,
            session: shared,
            reconnect_rx: Mutex::new(Some(reconnect_rx)),
            started: std::sync::atomic::AtomicBool::new(false),
            cancel_token: CancellationToken::new(),
            conn_tx,
            conn_rx,
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            last_avg_response_time_ms: AtomicU64::new(0),
        })
    }

    #[inline]
    /// Collect data with specified Modbus slave id for all points
    async fn collect_with_slave(
        &self,
        device_id: i32,
        device_name: &str,
        slave_id: u8,
        points_any: &[Arc<dyn RuntimePoint>],
    ) -> DriverResult<Vec<NorthwardData>> {
        // Downcast points to ModbusPoint and build batches
        let modbus_points: Vec<&ModbusPoint> = points_any
            .iter()
            .filter_map(|p| p.downcast_ref::<ModbusPoint>())
            .filter(|mp| matches!(mp.access_mode(), AccessMode::Read | AccessMode::ReadWrite))
            .collect();

        if modbus_points.is_empty() {
            return Ok(Vec::new());
        }

        let batches = ModbusPlanner::plan_read_batches(
            ModbusPlannerConfig {
                max_gap: self.inner.config.max_gap,
                max_batch: self.inner.config.max_batch.max(1),
            },
            &modbus_points,
        );

        let mut telemetry_values: Vec<PointValue> = Vec::with_capacity(modbus_points.len());
        let mut attribute_values: Vec<PointValue> = Vec::with_capacity(modbus_points.len());

        // Acquire context once for the collection cycle
        let ctx_arc = self.session.ctx.load_full();
        let Some(ctx_arc) = ctx_arc else {
            self.session.healthy.store(false, Ordering::Release);
            let _ = self
                .session
                .last_error
                .lock()
                .map(|mut g| *g = Some("no context".to_string()));
            let _ = self.session.reconnect_tx.try_send(());
            return Err(DriverError::ServiceUnavailable);
        };

        let slave = Slave(slave_id);
        let timeout_ms = self.inner.connection_policy.read_timeout_ms.max(1);

        for batch in batches {
            let op_label = match batch.function {
                ModbusFunctionCode::ReadCoils => "ReadCoils",
                ModbusFunctionCode::ReadDiscreteInputs => "ReadDiscreteInputs",
                ModbusFunctionCode::ReadHoldingRegisters => "ReadHoldingRegisters",
                ModbusFunctionCode::ReadInputRegisters => "ReadInputRegisters",
                _ => "UnknownRead",
            };

            // Validate function
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
                            match p.r#type() {
                                DataPointType::Telemetry => {
                                    let Some(value) =
                                        ValueCodec::coerce_bool_to_value(val, p.data_type, p.scale)
                                    else {
                                        warn!(
                                            point_id = p.id,
                                            key = %p.key,
                                            expected = ?p.data_type,
                                            "Failed to coerce coil value to NGValue; dropped"
                                        );
                                        continue;
                                    };
                                    telemetry_values.push(PointValue {
                                        point_id: p.id,
                                        point_key: Arc::<str>::from(p.key.as_str()),
                                        value,
                                    });
                                }
                                DataPointType::Attribute => {
                                    let Some(value) =
                                        ValueCodec::coerce_bool_to_value(val, p.data_type, p.scale)
                                    else {
                                        warn!(
                                            point_id = p.id,
                                            key = %p.key,
                                            expected = ?p.data_type,
                                            "Failed to coerce coil value to NGValue; dropped"
                                        );
                                        continue;
                                    };
                                    attribute_values.push(PointValue {
                                        point_id: p.id,
                                        point_key: Arc::<str>::from(p.key.as_str()),
                                        value,
                                    });
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
                            let slice = &words[offset..offset + qty];
                            match p.r#type() {
                                DataPointType::Telemetry => {
                                    let value = match ModbusCodec::parse_register_value(
                                        slice,
                                        p.data_type,
                                        self.inner.config.byte_order,
                                        self.inner.config.word_order,
                                        p.scale,
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
                                    telemetry_values.push(PointValue {
                                        point_id: p.id,
                                        point_key: Arc::<str>::from(p.key.as_str()),
                                        value,
                                    });
                                }
                                DataPointType::Attribute => {
                                    let value = match ModbusCodec::parse_register_value(
                                        slice,
                                        p.data_type,
                                        self.inner.config.byte_order,
                                        self.inner.config.word_order,
                                        p.scale,
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
                                    attribute_values.push(PointValue {
                                        point_id: p.id,
                                        point_key: Arc::<str>::from(p.key.as_str()),
                                        value,
                                    });
                                }
                            }
                        }
                    }
                },
                Err(e) => return Err(e),
            }
        }

        let mut out: Vec<NorthwardData> = Vec::with_capacity(2);
        if !telemetry_values.is_empty() {
            out.push(NorthwardData::Telemetry(TelemetryData::new(
                device_id,
                device_name.to_string(),
                telemetry_values,
            )));
        }
        if !attribute_values.is_empty() {
            out.push(NorthwardData::Attributes(
                AttributeData::new_client_attributes(
                    device_id,
                    device_name.to_string(),
                    attribute_values,
                ),
            ));
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
        supervisor.run(inner).await;
        Ok(())
    }

    #[instrument(level = "info", skip_all)]
    async fn stop(&self) -> DriverResult<()> {
        self.cancel_token.cancel();
        self.session.shutdown.store(true, Ordering::Release);
        if let Some(ctx) = self.session.ctx.swap(None) {
            let _ = timeout(TokioDuration::from_secs(2), async move {
                let mut guard = ctx.lock().await;
                guard.disconnect().await
            })
            .await;
        }
        Ok(())
    }

    #[inline]
    #[instrument(level = "debug", skip_all)]
    async fn collect_data(
        &self,
        device: Arc<dyn RuntimeDevice>,
        data_points: Arc<[Arc<dyn RuntimePoint>]>,
    ) -> DriverResult<Vec<NorthwardData>> {
        if let Some(md) = device.downcast_ref::<ModbusDevice>() {
            return self
                .collect_with_slave(md.id, &md.device_name, md.slave_id, data_points.as_ref())
                .await;
        }
        Err(DriverError::ConfigurationError(
            "RuntimeDevice is not ModbusDevice for ModbusDriver".to_string(),
        ))
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

        // Execute write plans sequentially using a single acquired context to preserve ordering
        let ctx_arc = self.session.ctx.load_full();
        let Some(ctx_arc) = ctx_arc else {
            self.session.healthy.store(false, Ordering::Release);
            let _ = self
                .session
                .last_error
                .lock()
                .map(|mut g| *g = Some("no context".to_string()));
            let _ = self.session.reconnect_tx.try_send(());
            return Err(DriverError::ServiceUnavailable);
        };

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

        // Acquire context snapshot.
        let ctx_arc = self.session.ctx.load_full();
        let Some(ctx_arc) = ctx_arc else {
            self.session.healthy.store(false, Ordering::Release);
            let _ = self
                .session
                .last_error
                .lock()
                .map(|mut g| *g = Some("no context".to_string()));
            let _ = self.session.reconnect_tx.try_send(());
            return Err(DriverError::ServiceUnavailable);
        };

        // Strict datatype guard (core should already validate, keep driver defensive).
        if !value.validate_datatype(point.data_type) {
            return Err(DriverError::ValidationError(format!(
                "type mismatch: expected {:?}, got {:?}",
                point.data_type,
                value.data_type()
            )));
        }

        let slave = Slave(device.slave_id);
        let address = point.address;

        // Encode and execute exactly one write op.
        match write_fc {
            ModbusFunctionCode::WriteSingleCoil | ModbusFunctionCode::WriteMultipleCoils => {
                if point.data_type != DataType::Boolean {
                    return Err(DriverError::ValidationError(format!(
                        "coil write expects Boolean data_type, got {:?}",
                        point.data_type
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
                    point.data_type,
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
