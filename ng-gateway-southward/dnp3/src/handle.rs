//! DNP3 southward data-plane handle.
//!
//! This is the **only** hot-path object published by the SDK supervision loop.
//! It provides downlink operations (actions + write point) and runtime delta application.
//!
//! Notes:
//! - DNP3 telemetry is primarily pushed by SOE callbacks (`Dnp3SoeHandler`) via `publisher`.
//! - `collect_data()` is a no-op for this driver (report-driven).

use super::types::{
    ControlCode as NgControlCode, Dnp3Action, Dnp3Channel, Dnp3CommandType, Dnp3Device,
    Dnp3Parameter, Dnp3Point, Dnp3PointGroup, PointMeta,
};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use dashmap::DashMap;
use dnp3::{
    app::{
        control::{
            CommandStatus, ControlCode as Dnp3ControlCode, Group12Var1, Group41Var1, Group41Var2,
            Group41Var3, Group41Var4,
        },
        Variation,
    },
    master::{
        AssociationHandle, CommandBuilder, CommandMode, CommandSupport as _, ReadHeader,
        ReadRequest,
    },
};
use ng_gateway_sdk::{
    downcast_parameters, AccessMode, CollectItem, DataType, DriverError, DriverResult,
    ExecuteOutcome, ExecuteResult, NGValue, NGValueCastError, NorthwardData, NorthwardPublisher,
    RuntimeAction, RuntimeDelta, RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardHandle,
    SouthwardInitContext, WriteOutcome, WriteResult,
};
use serde_json::json;
use std::sync::Arc;
use tracing::warn;

/// Shared association handle published by the supervision session.
pub type SharedAssociation = Arc<ArcSwap<Option<AssociationHandle>>>;

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
    match param.wire_data_type() {
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

/// DNP3 data-plane handle.
pub struct Dnp3Handle {
    pub inner: Arc<Dnp3Channel>,
    pub publisher: Arc<dyn NorthwardPublisher>,
    pub shared_association: SharedAssociation,
    pub points_map: Arc<DashMap<(Dnp3PointGroup, u16), PointMeta>>,
    /// Fast lookup to clean/update old group/index when a runtime point is updated.
    pub point_index_by_id: Arc<DashMap<i32, (Dnp3PointGroup, u16)>>,
    /// Device name cache keyed by device id for building PointMeta.
    pub device_name_index: Arc<DashMap<i32, Arc<str>>>,
}

impl Dnp3Handle {
    /// Create a new handle from init context (no I/O).
    pub fn new(inner: Arc<Dnp3Channel>, publisher: Arc<dyn NorthwardPublisher>) -> Self {
        Self {
            inner,
            publisher,
            shared_association: Arc::new(ArcSwap::from_pointee(None)),
            points_map: Arc::new(DashMap::new()),
            point_index_by_id: Arc::new(DashMap::new()),
            device_name_index: Arc::new(DashMap::new()),
        }
    }

    /// Build index structures from runtime topology (cold path).
    pub fn build_indexes(&self, ctx: &SouthwardInitContext) {
        for device in ctx.devices.iter() {
            if let Some(d) = device.downcast_ref::<Dnp3Device>() {
                let device_name: Arc<str> = Arc::from(d.device_name.as_str());
                self.device_name_index
                    .insert(d.id, Arc::clone(&device_name));
                if let Some(points) = ctx.points_by_device.get(&d.id) {
                    for p in points {
                        if let Some(dp) = p.downcast_ref::<Dnp3Point>() {
                            let meta = PointMeta {
                                point_id: dp.id,
                                key: Arc::from(dp.key.as_str()),
                                data_type: dp.wire_data_type(),
                                transform: dp.transform,
                                kind: dp.r#type,
                                device_id: d.id,
                                device_name: Arc::clone(&device_name),
                            };
                            self.points_map.insert((dp.group, dp.index), meta);
                            self.point_index_by_id.insert(dp.id, (dp.group, dp.index));
                        }
                    }
                }
            }
        }
    }

    #[inline]
    pub(crate) fn attach_association(&self, assoc: AssociationHandle) {
        self.shared_association.store(Arc::new(Some(assoc)));
    }

    #[inline]
    pub(crate) fn detach_association(&self) {
        self.shared_association.store(Arc::new(None));
    }

    #[inline]
    fn load_association(&self) -> DriverResult<AssociationHandle> {
        let g = self.shared_association.load();
        match g.as_ref() {
            Some(a) => Ok(a.clone()),
            None => Err(DriverError::ServiceUnavailable),
        }
    }
}

#[async_trait]
impl SouthwardHandle for Dnp3Handle {
    async fn collect_data(&self, _items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        // Report-driven; collector should not call this.
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

        let mut association = self.load_association()?;

        for (param, value) in resolved {
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
                    build_ao_command(&mut builder, &param, &value)?;
                    association
                        .operate(CommandMode::DirectOperate, builder.build())
                        .await
                        .map_err(|e| {
                            DriverError::ExecutionError(format!("Operate failed: {:?}", e))
                        })?;
                }
                Dnp3CommandType::WarmRestart => {
                    let _ = association.warm_restart().await.map_err(|e| {
                        DriverError::ExecutionError(format!("Warm restart failed: {:?}", e))
                    })?;
                }
                Dnp3CommandType::ColdRestart => {
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
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let point = point
            .downcast_ref::<Dnp3Point>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not Dnp3Point for Dnp3Handle".into(),
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

        if matches!(point.group, Dnp3PointGroup::BinaryOutput)
            && point.wire_data_type() != DataType::UInt8
        {
            return Err(DriverError::ValidationError(format!(
                "BinaryOutput WritePoint only supports UInt8 data_type (CROB control code), got {:?}",
                point.wire_data_type()
            )));
        }

        let mut association = self.load_association()?;

        let param = Dnp3Parameter {
            name: point.name.clone(),
            key: point.key.clone(),
            data_type: point.wire_data_type(),
            required: true,
            default_value: None,
            max_value: point.max_value,
            min_value: point.min_value,
            transform: point.transform,
            group: command_type,
            index: point.index,
            crob_count: None,
            crob_on_time_ms: None,
            crob_off_time_ms: None,
        };

        let mut builder = CommandBuilder::new();
        match command_type {
            Dnp3CommandType::CROB => build_crob_command(&mut builder, &param, value)?,
            Dnp3CommandType::AnalogOutputCommand => build_ao_command(&mut builder, &param, value)?,
            _ => unreachable!(),
        };

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
            Err(_) => return Err(DriverError::Timeout(timeout_duration)),
        }

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value.clone()),
        })
    }

    async fn apply_runtime_delta(&self, delta: RuntimeDelta) -> DriverResult<()> {
        let mut has_new_points = false;

        match delta {
            RuntimeDelta::DevicesChanged {
                added,
                updated,
                removed,
                status_changed: _,
            } => {
                for dev in added.iter().chain(updated.iter()) {
                    if let Some(d) = dev.downcast_ref::<Dnp3Device>() {
                        let name: Arc<str> = Arc::from(d.device_name.as_str());
                        self.device_name_index.insert(d.id, name);
                    } else {
                        warn!("Received non-DNP3 device in runtime delta (add/update)");
                    }
                }

                for dev in removed {
                    if let Some(d) = dev.downcast_ref::<Dnp3Device>() {
                        self.device_name_index.remove(&d.id);
                        let device_id = d.id;
                        self.points_map
                            .retain(|_, meta| meta.device_id != device_id);
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
                ..
            } => {
                let device_name = device
                    .downcast_ref::<Dnp3Device>()
                    .map(|d| Arc::from(d.device_name.as_str()));

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
                                data_type: dp.wire_data_type(),
                                transform: dp.transform,
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
                                data_type: dp.wire_data_type(),
                                transform: dp.transform,
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

        // Best-effort refresh after point add.
        if has_new_points {
            let g = self.shared_association.load();
            if let Some(assoc) = g.as_ref() {
                let mut assoc = assoc.clone();
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
}
