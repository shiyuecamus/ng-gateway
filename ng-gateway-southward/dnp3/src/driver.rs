use crate::{
    supervisor::{Dnp3Supervisor, SharedAssociation},
    types::{
        ControlCode as NgControlCode, Dnp3Action, Dnp3Channel, Dnp3CommandType, Dnp3Device,
        Dnp3Parameter, Dnp3Point, Dnp3PointGroup, PointMeta,
    },
};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use dashmap::DashMap;
use dnp3::{
    app::control::{
        CommandStatus, ControlCode as Dnp3ControlCode, Group12Var1, Group41Var1, Group41Var2,
        Group41Var3, Group41Var4,
    },
    app::Variation,
    master::{CommandBuilder, CommandMode, CommandSupport, ReadHeader, ReadRequest},
};
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, DataType, Driver, DriverError, DriverHealth, DriverResult,
    ExecuteOutcome, ExecuteResult, HealthStatus, NGValue, NGValueCastError, NorthwardData,
    NorthwardPublisher, RuntimeAction, RuntimeDelta, RuntimeDevice, RuntimeParameter, RuntimePoint,
    SouthwardConnectionState, SouthwardInitContext, WriteOutcome, WriteResult,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

#[inline]
fn parse_crob_control_code(value: &NGValue) -> DriverResult<Dnp3ControlCode> {
    let code_u8 = u8::try_from(value).map_err(|_| {
        DriverError::ValidationError(format!(
            "CROB control code out of range (0..=255): {:?}",
            value
        ))
    })?;

    let code = NgControlCode::try_from(code_u8).map_err(|e| {
        DriverError::ValidationError(format!(
            "Invalid CROB control code byte {code_u8:#04x}: {e}"
        ))
    })?;

    Ok(code.to_dnp3())
}

#[inline]
fn build_crob_command(
    builder: &mut CommandBuilder,
    param: &Dnp3Parameter,
    value: &NGValue,
) -> DriverResult<()> {
    let code = parse_crob_control_code(value)?;

    let count = param.crob_count.unwrap_or(1);
    if count == 0 {
        return Err(DriverError::ValidationError(
            "CROB count must be >= 1".to_string(),
        ));
    }

    let on_time = param.crob_on_time_ms.unwrap_or(0);
    let off_time = param.crob_off_time_ms.unwrap_or(0);

    let crob = Group12Var1 {
        code,
        count,
        on_time,
        off_time,
        status: CommandStatus::Success,
    };
    builder.add_u16(crob, param.index);
    Ok(())
}

#[inline]
fn build_ao_command(
    builder: &mut CommandBuilder,
    param: &Dnp3Parameter,
    value: &NGValue,
) -> DriverResult<()> {
    match param.data_type {
        DataType::Int16 | DataType::UInt16 => {
            let value = i16::try_from(value).map_err(|e: NGValueCastError| {
                DriverError::ValidationError(format!(
                    "AnalogOutputCommand expects numeric value, got {:?}: {e}",
                    value.data_type()
                ))
            })?;
            let ao = Group41Var2 {
                value,
                status: CommandStatus::Success,
            };
            builder.add_u16(ao, param.index);
        }
        DataType::Int32 | DataType::UInt32 => {
            let value = i32::try_from(value).map_err(|e: NGValueCastError| {
                DriverError::ValidationError(format!(
                    "AnalogOutputCommand expects numeric value, got {:?}: {e}",
                    value.data_type()
                ))
            })?;
            let ao = Group41Var1 {
                value,
                status: CommandStatus::Success,
            };
            builder.add_u16(ao, param.index);
        }
        DataType::Float32 => {
            let value = f32::try_from(value).map_err(|e: NGValueCastError| {
                DriverError::ValidationError(format!(
                    "AnalogOutputCommand expects numeric value, got {:?}: {e}",
                    value.data_type()
                ))
            })?;
            let ao = Group41Var3 {
                value,
                status: CommandStatus::Success,
            };
            builder.add_u16(ao, param.index);
        }
        DataType::Float64 => {
            let value = f64::try_from(value).map_err(|e: NGValueCastError| {
                DriverError::ValidationError(format!(
                    "AnalogOutputCommand expects numeric value, got {:?}: {e}",
                    value.data_type()
                ))
            })?;
            let ao = Group41Var4 {
                value,
                status: CommandStatus::Success,
            };
            builder.add_u16(ao, param.index);
        }
        other => {
            return Err(DriverError::ExecutionError(format!(
                "Unsupported DataType for AnalogOutputCommand: {:?}",
                other
            )));
        }
    }

    Ok(())
}

pub struct Dnp3Driver {
    pub inner: Arc<Dnp3Channel>,
    pub publisher: Arc<dyn NorthwardPublisher>,
    pub shared_association: SharedAssociation,
    pub points_map: Arc<DashMap<(Dnp3PointGroup, u16), PointMeta>>,
    /// Fast lookup to clean/update old group/index when a runtime point is updated.
    pub point_index_by_id: Arc<DashMap<i32, (Dnp3PointGroup, u16)>>,
    /// Device name cache keyed by device id for building PointMeta.
    pub device_name_index: Arc<DashMap<i32, Arc<str>>>,

    started: AtomicBool,
    cancel_token: CancellationToken,

    conn_tx: watch::Sender<SouthwardConnectionState>,
    conn_rx: watch::Receiver<SouthwardConnectionState>,
}

impl Dnp3Driver {
    pub fn with_context(ctx: SouthwardInitContext) -> DriverResult<Self> {
        let inner = ctx
            .runtime_channel
            .downcast_arc::<Dnp3Channel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid Dnp3Channel".to_string()))?;

        // Build points map
        let points_map = Arc::new(DashMap::new());
        let point_index_by_id = Arc::new(DashMap::new());
        let device_name_index = Arc::new(DashMap::new());
        for device in ctx.devices.iter() {
            if let Some(d) = device.downcast_ref::<Dnp3Device>() {
                // Share the device name across all points of the same device to avoid
                // cloning the same `String` N times (N = number of points).
                let device_name: Arc<str> = Arc::from(d.device_name.as_str());
                device_name_index.insert(d.id, Arc::clone(&device_name));
                if let Some(points) = ctx.points_by_device.get(&d.id) {
                    for p in points {
                        if let Some(dp) = p.downcast_ref::<Dnp3Point>() {
                            let meta = PointMeta {
                                point_id: dp.id,
                                key: Arc::from(dp.key.as_str()),
                                data_type: dp.data_type,
                                scale: dp.scale,
                                kind: dp.r#type,
                                device_id: d.id,
                                device_name: Arc::clone(&device_name),
                            };
                            points_map.insert((dp.group, dp.index), meta);
                            point_index_by_id.insert(dp.id, (dp.group, dp.index));
                        }
                    }
                }
            }
        }

        let (conn_tx, conn_rx) = watch::channel(SouthwardConnectionState::Disconnected);

        Ok(Self {
            inner,
            publisher: ctx.publisher,
            shared_association: Arc::new(ArcSwap::from_pointee(None)),
            points_map,
            point_index_by_id,
            device_name_index,
            started: AtomicBool::new(false),
            cancel_token: CancellationToken::new(),
            conn_tx,
            conn_rx,
        })
    }

    /// Wait for association to be available.
    ///
    /// This method polls the shared_association until it becomes available,
    /// with a timeout to avoid infinite waiting.
    ///
    /// # Arguments
    /// * `timeout` - Maximum duration to wait for association
    ///
    /// # Returns
    /// * `Ok(())` if association becomes available within timeout
    /// * `Err(DriverError::ServiceUnavailable)` if timeout is reached
    pub async fn wait_association(&self, timeout: std::time::Duration) -> DriverResult<()> {
        let start = std::time::Instant::now();
        let poll_interval = std::time::Duration::from_millis(100);

        loop {
            if self.shared_association.load().is_some() {
                return Ok(());
            }

            if start.elapsed() >= timeout {
                return Err(DriverError::ServiceUnavailable);
            }

            tokio::time::sleep(poll_interval).await;
        }
    }
}

#[async_trait]
impl Driver for Dnp3Driver {
    async fn start(&self) -> DriverResult<()> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let supervisor = Dnp3Supervisor::new(
            Arc::clone(&self.inner),
            Arc::clone(&self.points_map),
            Arc::clone(&self.publisher),
            Arc::clone(&self.shared_association),
            self.conn_tx.clone(),
            self.cancel_token.child_token(),
        );
        supervisor.run().await?;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn stop(&self) -> DriverResult<()> {
        self.cancel_token.cancel();
        self.started.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn collect_data(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        _data_points: Arc<[Arc<dyn RuntimePoint>]>,
    ) -> DriverResult<Vec<NorthwardData>> {
        // TODO: Report-only; collector shouldn't call into this for DNP3
        Ok(Vec::new())
    }

    async fn execute_action(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let action = action
            .downcast_ref::<Dnp3Action>()
            .ok_or(DriverError::ConfigurationError("Not Dnp3Action".into()))?;

        let resolved = downcast_parameters::<Dnp3Parameter>(parameters)?;

        // Load association
        let association_guard = self.shared_association.load();
        let mut association = match association_guard.as_ref() {
            Some(assoc) => assoc.clone(),
            None => return Err(DriverError::ServiceUnavailable),
        };

        // Iterate over all resolved parameters and execute the corresponding command.
        // For CROB / AnalogOutputCommand we continue to use the DNP3 command builder,
        // while restart-style commands are mapped to the dedicated restart tasks on
        // the association handle.
        for (param, value) in resolved {
            // Group 12: CROB
            // Group 41: Analog Output
            match param.group {
                Dnp3CommandType::CROB => {
                    let mut builder = CommandBuilder::new();
                    build_crob_command(&mut builder, &param, &value)?;

                    association
                        .operate(CommandMode::DirectOperate, builder.build())
                        .await
                        .map_err(|e| {
                            DriverError::ExecutionError(format!("Operate failed: {:?}", e))
                        })?;
                }
                Dnp3CommandType::AnalogOutputCommand => {
                    let mut builder = CommandBuilder::new();
                    // Build the analog output command using the parameter's DataType mapping.
                    build_ao_command(&mut builder, &param, &value)?;
                    association
                        .operate(CommandMode::DirectOperate, builder.build())
                        .await
                        .map_err(|e| {
                            DriverError::ExecutionError(format!("Operate failed: {:?}", e))
                        })?;
                }
                Dnp3CommandType::WarmRestart => {
                    // Warm restart uses the dedicated DNP3 master API and does not require
                    // any object headers. The value, if present, is ignored.
                    let _ = association.warm_restart().await.map_err(|e| {
                        DriverError::ExecutionError(format!("Warm restart failed: {:?}", e))
                    })?;
                }
                Dnp3CommandType::ColdRestart => {
                    // Cold restart uses the dedicated DNP3 master API and does not require
                    // any object headers. The value, if present, is ignored.
                    let _ = association.cold_restart().await.map_err(|e| {
                        DriverError::ExecutionError(format!("Cold restart failed: {:?}", e))
                    })?;
                }
            }
        }

        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(json!(format!("Action '{}' executed", action.name()))),
        })
    }

    async fn write_point(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let point = point
            .downcast_ref::<Dnp3Point>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not Dnp3Point for Dnp3Driver".into(),
            ))?;

        if !matches!(point.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            return Err(DriverError::ValidationError(
                "point is not writeable".to_string(),
            ));
        }

        // Always enforce a timeout for DNP3 control operations (no protocol-layer per-call timeout here).
        let effective_timeout_ms = timeout_ms
            .unwrap_or(self.inner.connection_policy.write_timeout_ms)
            .max(1);
        let timeout_duration = tokio::time::Duration::from_millis(effective_timeout_ms);

        let command_type = match point.group {
            Dnp3PointGroup::BinaryOutput => Dnp3CommandType::CROB,
            Dnp3PointGroup::AnalogOutput => Dnp3CommandType::AnalogOutputCommand,
            other => {
                return Err(DriverError::ConfigurationError(format!(
                    "DNP3 point group {:?} is not supported for write_point",
                    other
                )))
            }
        };

        // Product-grade constraint (Option A):
        // WritePoint for BinaryOutput only supports numeric CROB control code (UInt8).
        if matches!(point.group, Dnp3PointGroup::BinaryOutput) && point.data_type != DataType::UInt8
        {
            return Err(DriverError::ValidationError(format!(
                "BinaryOutput WritePoint only supports UInt8 data_type (CROB control code), got {:?}",
                point.data_type
            )));
        }

        // Strict datatype guard (core should already validate, keep driver defensive).
        if !value.validate_datatype(point.data_type) {
            return Err(DriverError::ValidationError(format!(
                "type mismatch: expected {:?}, got {:?}",
                point.data_type,
                value.data_type()
            )));
        }

        // Load association (must be connected).
        let association_guard = self.shared_association.load();
        let mut association = match association_guard.as_ref() {
            Some(assoc) => assoc.clone(),
            None => return Err(DriverError::ServiceUnavailable),
        };

        let param = Dnp3Parameter {
            name: point.name.clone(),
            key: point.key.clone(),
            data_type: point.data_type,
            required: true,
            default_value: None,
            max_value: point.max_value,
            min_value: point.min_value,
            group: command_type,
            index: point.index,
            // WritePoint path intentionally does not expose CROB timing/count knobs.
            // For full control (count/on/off time), use Action inputs (`Dnp3Parameter`).
            crob_count: None,
            crob_on_time_ms: None,
            crob_off_time_ms: None,
        };

        let mut builder = CommandBuilder::new();
        match command_type {
            Dnp3CommandType::CROB => build_crob_command(&mut builder, &param, &value)?,
            Dnp3CommandType::AnalogOutputCommand => build_ao_command(&mut builder, &param, &value)?,
            _ => unreachable!(),
        };

        // Use the driver runtime timeout wrapper. The future must be `'static`, so move owned
        // association + request into `async move`.
        match tokio::time::timeout(timeout_duration, async move {
            association
                .operate(CommandMode::DirectOperate, builder.build())
                .await
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                return Err(DriverError::ExecutionError(format!(
                    "Operate failed: {:?}",
                    e
                )))
            }
            Err(_elapsed) => return Err(DriverError::Timeout(timeout_duration)),
        }

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value),
        })
    }

    /// Apply runtime deltas to keep points_map in sync and eagerly refresh data for newly added points.
    async fn apply_runtime_delta(&self, delta: RuntimeDelta) -> DriverResult<()> {
        let mut has_new_points = false;

        match delta {
            RuntimeDelta::DevicesChanged {
                added,
                updated,
                removed,
                status_changed: _,
            } => {
                // Add or update device name cache
                for dev in added.iter().chain(updated.iter()) {
                    if let Some(d) = dev.downcast_ref::<Dnp3Device>() {
                        let name: Arc<str> = Arc::from(d.device_name.as_str());
                        self.device_name_index.insert(d.id, name);
                    } else {
                        warn!("Received non-DNP3 device in runtime delta (add/update)");
                    }
                }

                // Remove devices and their points
                for dev in removed {
                    if let Some(d) = dev.downcast_ref::<Dnp3Device>() {
                        self.device_name_index.remove(&d.id);
                        let device_id = d.id;
                        // Remove points from points_map
                        self.points_map
                            .retain(|_, meta| meta.device_id != device_id);
                        // Clean point_index_by_id entries whose target points were removed
                        self.point_index_by_id
                            .retain(|_, (g, i)| self.points_map.contains_key(&(*g, *i)));
                    } else {
                        warn!("Received non-DNP3 device in runtime delta (remove)");
                    }
                }
            }
            RuntimeDelta::PointsChanged {
                device,
                added,
                updated,
                removed,
            } => {
                let device_name = device
                    .downcast_ref::<Dnp3Device>()
                    .map(|d| Arc::from(d.device_name.as_str()));

                // Remove old points
                for p in removed {
                    if let Some(dp) = p.downcast_ref::<Dnp3Point>() {
                        if let Some((_, (old_group, old_index))) =
                            self.point_index_by_id.remove(&dp.id)
                        {
                            self.points_map.remove(&(old_group, old_index));
                        }
                        self.points_map.remove(&(dp.group, dp.index));
                    } else {
                        warn!("Received non-DNP3 point in runtime delta (remove)");
                    }
                }

                // Apply updated points (remove old mapping if group/index changed)
                for p in updated {
                    if let Some(dp) = p.downcast_ref::<Dnp3Point>() {
                        if let Some(old_ref) = self.point_index_by_id.get(&dp.id) {
                            let (old_group, old_index) = *old_ref;
                            if old_group != dp.group || old_index != dp.index {
                                self.points_map.remove(&(old_group, old_index));
                            }
                        }
                        let dev_name = device_name.as_ref().cloned().or_else(|| {
                            self.device_name_index
                                .get(&dp.device_id)
                                .map(|v| Arc::clone(v.value()))
                        });
                        if let Some(name) = dev_name {
                            let meta = PointMeta {
                                point_id: dp.id,
                                key: Arc::from(dp.key.as_str()),
                                data_type: dp.data_type,
                                scale: dp.scale,
                                kind: dp.r#type,
                                device_id: dp.device_id,
                                device_name: name,
                            };
                            self.points_map.insert((dp.group, dp.index), meta);
                            self.point_index_by_id.insert(dp.id, (dp.group, dp.index));
                        } else {
                            warn!(
                                device_id = dp.device_id,
                                "Device name not found for updated point"
                            );
                        }
                    } else {
                        warn!("Received non-DNP3 point in runtime delta (update)");
                    }
                }

                // Add new points
                for p in added {
                    if let Some(dp) = p.downcast_ref::<Dnp3Point>() {
                        let dev_name = device_name.as_ref().cloned().or_else(|| {
                            self.device_name_index
                                .get(&dp.device_id)
                                .map(|v| Arc::clone(v.value()))
                        });
                        if let Some(name) = dev_name {
                            let meta = PointMeta {
                                point_id: dp.id,
                                key: Arc::from(dp.key.as_str()),
                                data_type: dp.data_type,
                                scale: dp.scale,
                                kind: dp.r#type,
                                device_id: dp.device_id,
                                device_name: name,
                            };
                            self.points_map.insert((dp.group, dp.index), meta);
                            self.point_index_by_id.insert(dp.id, (dp.group, dp.index));
                            has_new_points = true;
                        } else {
                            warn!(
                                device_id = dp.device_id,
                                "Device name not found for added point"
                            );
                        }
                    } else {
                        warn!("Received non-DNP3 point in runtime delta (add)");
                    }
                }
            }
            _ => {}
        }

        // Trigger a lightweight read to populate newly added points so that Web UI can see data promptly.
        if has_new_points {
            let guard = self.shared_association.load();
            if let Some(assoc) = guard.as_ref() {
                let mut assoc = assoc.clone();
                // Reuse integrity headers; callers are responsible for rate-limiting deltas.
                let headers = vec![
                    ReadHeader::all_objects(Variation::Group60Var1),
                    ReadHeader::all_objects(Variation::Group60Var2),
                    ReadHeader::all_objects(Variation::Group60Var3),
                    ReadHeader::all_objects(Variation::Group60Var4),
                    ReadHeader::all_objects(Variation::Group110(0)),
                ];

                let req = ReadRequest::multiple_headers(&headers);
                if let Err(e) = assoc.read(req).await {
                    warn!("DNP3 refresh read after point add failed: {:?}", e);
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> DriverResult<DriverHealth> {
        let connected = self.shared_association.load().is_some();
        Ok(DriverHealth {
            status: if connected {
                HealthStatus::Healthy
            } else {
                HealthStatus::Unhealthy
            },
            last_activity: chrono::Utc::now(),
            error_count: 0,
            success_rate: 1.0,
            average_response_time: std::time::Duration::from_millis(0),
            details: None,
        })
    }

    fn subscribe_connection_state(&self) -> watch::Receiver<SouthwardConnectionState> {
        self.conn_rx.clone()
    }
}
