use super::{
    protocol::{
        Asdu, BitsString32CommandInfo, Cause, CauseOfTransmission, DoubleCommandInfo, ObjectQCC,
        ObjectQOI, Session, SetPointCommandFloatInfo, SetPointCommandNormalInfo,
        SetPointCommandScaledInfo, SingleCommandInfo, StepCommandInfo, TypeID,
    },
    types::{Iec104Action, Iec104Channel, Iec104Device, Iec104Parameter, Iec104Point},
};
use arc_swap::{ArcSwap, ArcSwapOption};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use ng_gateway_sdk::{
    downcast_parameters, supervision::ReconnectHandle, AccessMode, AttributeData, CollectItem,
    DataPointType, DataType, DriverError, DriverResult, ExecuteOutcome, ExecuteResult, NGValue,
    NGValueCastError, NorthwardData, NorthwardPublisher, PointValue, RuntimeAction, RuntimeDelta,
    RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardHandle, SouthwardInitContext,
    TelemetryData, Transform, ValueCodec, WriteOutcome, WriteResult,
};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::time::Duration as TokioDuration;
use tracing::instrument;

/// Startup action item describing which interrogation(s) to trigger when session becomes Active.
#[derive(Clone)]
pub enum StartupAction {
    /// Trigger Counter Interrogation with given QCC.
    CounterInterrogation { ca: u16, qcc: u8 },
    /// Trigger General Interrogation with given QOI.
    GeneralInterrogation { ca: u16, qoi: u8 },
}

/// IEC 60870-5-104 data-plane handle (published when Connected/Ready).
///
/// - Single TCP session per channel (remote station)
/// - Report-only collection model: publish incoming ASDUs through northbound pipeline
/// - Optional startup sequence: Counter/General Interrogation when Active
pub struct Iec104Handle {
    /// Config
    inner: Arc<Iec104Channel>,
    /// Publisher for northward data
    publisher: Arc<dyn NorthwardPublisher>,
    /// Current active protocol session (present only when Connected/Ready).
    session: ArcSwapOption<crate::protocol::session::Session>,
    /// Best-effort reconnect request handle (set on each attempt).
    reconnect: ArcSwapOption<ng_gateway_sdk::supervision::ReconnectHandle>,
    /// CA -> CaSnapshot (supports multiple devices per CA)
    ca_to_snapshot: Arc<DashMap<u16, ArcSwap<CaSnapshot>>>,
    /// Metrics
    total_requests: Arc<AtomicU64>,
    successful_requests: Arc<AtomicU64>,
    failed_requests: Arc<AtomicU64>,
    last_avg_response_time_ms: Arc<AtomicU64>,
}

// Point metadata with device information
#[derive(Debug, Clone)]
struct PointMeta {
    point_id: i32,
    device_id: i32,
    device_name: Arc<str>,
    /// Stable point key within the device.
    ///
    /// This is used to populate `PointValue.point_key` so northward plugins can avoid
    /// repeated `point_id -> PointMeta` lookups.
    point_key: Arc<str>,
    data_type: DataType,
    transform: Transform,
    /// Data point kind used to decide publishing channel
    kind: DataPointType,
}

impl PointMeta {
    /// Get the wire data type (protocol-level, memory-layout semantics).
    #[inline]
    fn wire_data_type(&self) -> DataType {
        self.data_type
    }

    /// Get the logical data type (gateway-facing semantics).
    ///
    /// This is derived from `transform.transform_data_type` and falls back to the
    /// wire data type when not configured.
    #[inline]
    fn logical_data_type(&self) -> DataType {
        self.transform
            .resolve_logical_datatype(self.wire_data_type())
    }
}

// Points keyed by packed u32: high 8 bits = type_id, low 16 bits = ioa
type PointsMap = HashMap<u32, PointMeta>;

// Device values map: device_id -> (device_name, telemetry_values, attribute_values)
type DeviceValuesMap = HashMap<i32, (Arc<str>, Vec<PointValue>, Vec<PointValue>)>;

#[derive(Debug, Clone)]
pub(crate) struct CaSnapshot {
    /// Points map: (type_id, ioa) -> PointMeta
    points: PointsMap,
    /// Point ID -> packed key mapping for efficient updates
    id_index: HashMap<i32, u32>,
}

#[inline]
fn pack_key(ioa: u16, type_id: u8) -> u32 {
    ((type_id as u32) << 16) | (ioa as u32)
}

impl Iec104Handle {
    /// Create a new IEC104 handle (no I/O).
    ///
    /// # Notes
    /// - This is the data-plane handle published by the SDK `SupervisorLoop`.
    /// - The active protocol session is attached/detached by the supervised session lifecycle.
    pub fn new(
        inner: Arc<Iec104Channel>,
        publisher: Arc<dyn NorthwardPublisher>,
        ca_to_snapshot: Arc<DashMap<u16, ArcSwap<CaSnapshot>>>,
    ) -> Self {
        Self {
            inner,
            publisher,
            session: ArcSwapOption::from(None),
            reconnect: ArcSwapOption::from(None),
            ca_to_snapshot,
            last_avg_response_time_ms: Arc::new(AtomicU64::new(0)),
            total_requests: Arc::new(AtomicU64::new(0)),
            successful_requests: Arc::new(AtomicU64::new(0)),
            failed_requests: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Attach or detach the current active protocol session.
    #[inline]
    pub fn attach_session(&self, session: Option<Arc<Session>>) {
        self.session.store(session);
    }

    /// Set or clear the reconnect handle for this attempt.
    #[inline]
    pub fn set_reconnect_handle(&self, h: Option<ReconnectHandle>) {
        self.reconnect.store(h.map(Arc::new));
    }

    /// Best-effort request to reconnect the current attempt.
    #[allow(unused)]
    #[inline]
    pub fn request_reconnect(&self, reason: impl Into<Arc<str>>) -> bool {
        self.reconnect
            .load()
            .as_ref()
            .map(|h| h.try_request_reconnect(reason))
            .unwrap_or(false)
    }

    #[inline]
    pub fn build_mappings(ctx: &SouthwardInitContext) -> Arc<DashMap<u16, ArcSwap<CaSnapshot>>> {
        // Build CA -> CaSnapshot mapping, aggregating points from all devices sharing the same CA
        let ca_to_snapshot = Arc::new(DashMap::new());

        // First pass: collect all devices grouped by CA
        let mut ca_devices: HashMap<u16, Vec<&Iec104Device>> = HashMap::new();
        for d in ctx
            .devices
            .iter()
            .filter_map(|dev| dev.downcast_ref::<Iec104Device>())
        {
            ca_devices.entry(d.ca).or_default().push(d);
        }

        // Second pass: build snapshot for each CA, merging points from all devices
        for (ca, devices) in ca_devices {
            let mut points: PointsMap = HashMap::new();
            let mut id_index: HashMap<i32, u32> = HashMap::new();

            for d in devices {
                let dev_id = d.id;
                if let Some(device_points) = ctx.points_by_device.get(&dev_id) {
                    for p in device_points.iter() {
                        if let Some(ip) = p.downcast_ref::<Iec104Point>() {
                            let k = pack_key(ip.ioa, ip.type_id);

                            // Warn if duplicate (type_id, ioa) found for different devices
                            if let Some(existing) = points.get(&k) {
                                if existing.device_id != dev_id {
                                    tracing::warn!(
                                      "Duplicate (type_id={}, ioa={}) found for CA {}: device {} vs device {}, keeping first",
                                      ip.type_id, ip.ioa, ca, existing.device_id, dev_id
                                  );
                                    continue; // Keep first, skip duplicate
                                }
                            }

                            points.insert(
                                k,
                                PointMeta {
                                    point_id: p.id(),
                                    device_id: dev_id,
                                    device_name: Arc::<str>::from(d.device_name.clone()),
                                    point_key: Arc::<str>::from(p.key()),
                                    data_type: ip.data_type,
                                    transform: ip.transform,
                                    kind: ip.r#type,
                                },
                            );
                            id_index.insert(p.id(), k);
                        }
                    }
                }
            }

            let snapshot = ArcSwap::from_pointee(CaSnapshot { points, id_index });
            ca_to_snapshot.insert(ca, snapshot);
        }

        ca_to_snapshot
    }

    #[inline]
    pub fn build_startup_actions(&self) -> Vec<StartupAction> {
        let mut actions = Vec::new();
        for kv in self.ca_to_snapshot.iter() {
            let ca = *kv.key();
            if self.inner.config.auto_startup_counter_interrogation {
                actions.push(StartupAction::CounterInterrogation {
                    ca,
                    qcc: self.inner.config.startup_qcc,
                });
            }
            if self.inner.config.auto_startup_general_interrogation {
                actions.push(StartupAction::GeneralInterrogation {
                    ca,
                    qoi: self.inner.config.startup_qoi,
                });
            }
        }
        actions
    }

    pub(crate) async fn spawn_session_with_timeout<F, T, E>(
        timeout_ms: u64,
        fut: F,
    ) -> DriverResult<T>
    where
        F: Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        let timeout_duration = TokioDuration::from_millis(timeout_ms);

        match tokio::time::timeout(timeout_duration, fut).await {
            Ok(result) => match result {
                Ok(value) => Ok(value),
                Err(err) => Err(DriverError::ExecutionError(err.to_string())),
            },
            Err(_elapsed) => Err(DriverError::Timeout(timeout_duration)),
        }
    }

    #[inline]
    fn extract_values_by_kind(points_map: &PointsMap, asdu: &mut Asdu) -> DeviceValuesMap {
        // Returns: device_id -> (device_name, telemetry_values, attribute_values)
        let mut per_device: DeviceValuesMap = HashMap::new();
        let type_id_byte = asdu.identifier.type_id as u8;

        match asdu.identifier.type_id {
            TypeID::M_BO_NA_1 | TypeID::M_BO_TA_1 | TypeID::M_BO_TB_1 => {
                if let Ok(infos) = asdu.get_bit_string() {
                    for mut info in infos {
                        let ioa = info.ioa.addr().get();
                        if let Some(meta) = points_map.get(&pack_key(ioa, type_id_byte)) {
                            let v = info.bsi;
                            if let Some(value) = ValueCodec::coerce_u64_to_value(
                                v as u64,
                                meta.data_type,
                                &meta.transform,
                            ) {
                                let entry = per_device.entry(meta.device_id).or_insert((
                                    Arc::clone(&meta.device_name),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                                match meta.kind {
                                    DataPointType::Telemetry => {
                                        entry.1.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                    DataPointType::Attribute => {
                                        entry.2.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            TypeID::M_ST_NA_1 | TypeID::M_ST_TA_1 | TypeID::M_ST_TB_1 => {
                if let Ok(infos) = asdu.get_step_position() {
                    for mut info in infos {
                        let ioa = info.ioa.addr().get();
                        if let Some(meta) = points_map.get(&pack_key(ioa, type_id_byte)) {
                            let v = info.vti.value().get();
                            if let Some(value) = ValueCodec::coerce_i64_to_value(
                                v.value() as i64,
                                meta.data_type,
                                &meta.transform,
                            ) {
                                let entry = per_device.entry(meta.device_id).or_insert((
                                    Arc::clone(&meta.device_name),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                                match meta.kind {
                                    DataPointType::Telemetry => {
                                        entry.1.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                    DataPointType::Attribute => {
                                        entry.2.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            TypeID::M_SP_NA_1 | TypeID::M_SP_TA_1 | TypeID::M_SP_TB_1 => {
                if let Ok(infos) = asdu.get_single_point() {
                    for mut info in infos {
                        let ioa = info.ioa.addr().get();
                        if let Some(meta) = points_map.get(&pack_key(ioa, type_id_byte)) {
                            let v = info.siq.spi().get();
                            if let Some(value) = ValueCodec::coerce_bool_to_value(
                                v,
                                meta.logical_data_type(),
                                &meta.transform,
                            ) {
                                let entry = per_device.entry(meta.device_id).or_insert((
                                    Arc::clone(&meta.device_name),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                                match meta.kind {
                                    DataPointType::Telemetry => {
                                        entry.1.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                    DataPointType::Attribute => {
                                        entry.2.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            TypeID::M_DP_NA_1 | TypeID::M_DP_TA_1 | TypeID::M_DP_TB_1 => {
                if let Ok(infos) = asdu.get_double_point() {
                    for mut info in infos {
                        let ioa = info.ioa.addr().get();
                        if let Some(meta) = points_map.get(&pack_key(ioa, type_id_byte)) {
                            let v: u8 = info.diq.spi().get().value();
                            if let Some(value) = ValueCodec::coerce_u64_to_value(
                                v as u64,
                                meta.data_type,
                                &meta.transform,
                            ) {
                                let entry = per_device.entry(meta.device_id).or_insert((
                                    meta.device_name.clone(),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                                match meta.kind {
                                    DataPointType::Telemetry => {
                                        entry.1.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                    DataPointType::Attribute => {
                                        entry.2.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            TypeID::M_ME_NC_1 | TypeID::M_ME_TC_1 | TypeID::M_ME_TF_1 => {
                if let Ok(infos) = asdu.get_measured_value_float() {
                    for mut info in infos {
                        let ioa = info.ioa.addr().get();
                        if let Some(meta) = points_map.get(&pack_key(ioa, type_id_byte)) {
                            let v = info.r as f64;
                            if let Some(value) = ValueCodec::coerce_f64_to_value(
                                v,
                                meta.logical_data_type(),
                                &meta.transform,
                            ) {
                                let entry = per_device.entry(meta.device_id).or_insert((
                                    meta.device_name.clone(),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                                match meta.kind {
                                    DataPointType::Telemetry => {
                                        entry.1.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                    DataPointType::Attribute => {
                                        entry.2.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            TypeID::M_ME_NA_1 | TypeID::M_ME_TA_1 | TypeID::M_ME_TD_1 | TypeID::M_ME_ND_1 => {
                if let Ok(infos) = asdu.get_measured_value_normal() {
                    for mut info in infos {
                        let ioa = info.ioa.addr().get();
                        if let Some(meta) = points_map.get(&pack_key(ioa, type_id_byte)) {
                            let v = info.value();
                            if let Some(value) = ValueCodec::coerce_f64_to_value(
                                v,
                                meta.logical_data_type(),
                                &meta.transform,
                            ) {
                                let entry = per_device.entry(meta.device_id).or_insert((
                                    meta.device_name.clone(),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                                match meta.kind {
                                    DataPointType::Telemetry => {
                                        entry.1.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                    DataPointType::Attribute => {
                                        entry.2.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            TypeID::M_ME_NB_1 | TypeID::M_ME_TB_1 | TypeID::M_ME_TE_1 => {
                if let Ok(infos) = asdu.get_measured_value_scaled() {
                    for mut info in infos {
                        let ioa = info.ioa.addr().get();
                        if let Some(meta) = points_map.get(&pack_key(ioa, type_id_byte)) {
                            let v = info.sva as f64;
                            if let Some(value) = ValueCodec::coerce_f64_to_value(
                                v,
                                meta.logical_data_type(),
                                &meta.transform,
                            ) {
                                let entry = per_device.entry(meta.device_id).or_insert((
                                    meta.device_name.clone(),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                                match meta.kind {
                                    DataPointType::Telemetry => {
                                        entry.1.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                    DataPointType::Attribute => {
                                        entry.2.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            TypeID::M_IT_NA_1 | TypeID::M_IT_TA_1 | TypeID::M_IT_TB_1 => {
                if let Ok(infos) = asdu.get_integrated_totals() {
                    for mut info in infos {
                        let ioa = info.ioa.addr().get();
                        if let Some(meta) = points_map.get(&pack_key(ioa, type_id_byte)) {
                            let v = info.bcr.value as i64;
                            if let Some(value) = ValueCodec::coerce_i64_to_value(
                                v,
                                meta.logical_data_type(),
                                &meta.transform,
                            ) {
                                let entry = per_device.entry(meta.device_id).or_insert((
                                    meta.device_name.clone(),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                                match meta.kind {
                                    DataPointType::Telemetry => {
                                        entry.1.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                    DataPointType::Attribute => {
                                        entry.2.push(PointValue {
                                            point_id: meta.point_id,
                                            point_key: Arc::clone(&meta.point_key),
                                            value,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        per_device
    }

    /// Process a single incoming ASDU and publish northward data.
    ///
    /// # Notes
    /// - This is part of the session `run()` receive loop (control-plane).
    /// - Publishing is best-effort; backpressure errors are accounted in metrics.
    pub fn process_asdu(&self, asdu: &mut Asdu) {
        let start_ts = Instant::now();
        let ca = asdu.identifier.common_addr;
        let Some(snapshot_swap) = self.ca_to_snapshot.get(&ca) else {
            return;
        };
        let snapshot = snapshot_swap.load();
        let per_device = Self::extract_values_by_kind(&snapshot.points, asdu);

        for (device_id, (device_name, telemetry_values, attribute_values)) in per_device {
            if !telemetry_values.is_empty() {
                let data = NorthwardData::Telemetry(TelemetryData::new(
                    device_id,
                    device_name.to_string(),
                    telemetry_values,
                ));
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                match self.publisher.try_publish(Arc::new(data)) {
                    Ok(()) => {
                        self.successful_requests.fetch_add(1, Ordering::Relaxed);
                        let prev = self.last_avg_response_time_ms.load(Ordering::Acquire);
                        let elapsed_ms = start_ts.elapsed().as_millis() as u64;
                        let new_avg = if prev == 0 {
                            elapsed_ms
                        } else {
                            (prev.saturating_mul(9) + elapsed_ms) / 10
                        };
                        self.last_avg_response_time_ms
                            .store(new_avg, Ordering::Release);
                    }
                    Err(_e) => {
                        self.failed_requests.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }

            if !attribute_values.is_empty() {
                let data = NorthwardData::Attributes(AttributeData::new_client_attributes(
                    device_id,
                    device_name.to_string(),
                    attribute_values,
                ));
                self.total_requests.fetch_add(1, Ordering::Relaxed);
                match self.publisher.try_publish(Arc::new(data)) {
                    Ok(()) => {
                        self.successful_requests.fetch_add(1, Ordering::Relaxed);
                        let prev = self.last_avg_response_time_ms.load(Ordering::Acquire);
                        let elapsed_ms = start_ts.elapsed().as_millis() as u64;
                        let new_avg = if prev == 0 {
                            elapsed_ms
                        } else {
                            (prev.saturating_mul(9) + elapsed_ms) / 10
                        };
                        self.last_avg_response_time_ms
                            .store(new_avg, Ordering::Release);
                    }
                    Err(_e) => {
                        self.failed_requests.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

#[async_trait]
impl SouthwardHandle for Iec104Handle {
    #[instrument(level = "debug", skip_all)]
    async fn collect_data(&self, _items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        // TODO: Report-only; collector shouldn't call into this for IEC104
        Ok(Vec::new())
    }

    #[instrument(level = "debug", skip_all)]
    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let action =
            action
                .downcast_ref::<Iec104Action>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeAction is not Iec104Action".to_string(),
                ))?;
        let device =
            device
                .downcast_ref::<Iec104Device>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not Iec104Device".to_string(),
                ))?;

        let resolved = downcast_parameters::<Iec104Parameter>(parameters)?;

        let session = self
            .session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)?;

        // Common constants
        let ca: u16 = device.ca;
        let cot = CauseOfTransmission::new(false, false, Cause::Activation);
        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);

        for (p, value) in resolved.into_iter() {
            // Validate IOA
            let type_id = TypeID::try_from(p.type_id).map_err(|_| {
                DriverError::ConfigurationError(format!("Unknown IEC104 type id: {}", p.type_id))
            })?;

            let start_ts = Instant::now();
            let fut_res: DriverResult<()> = match type_id {
                TypeID::C_SC_NA_1 | TypeID::C_SC_TA_1 => {
                    let b: bool = (&value).try_into().map_err(|e: NGValueCastError| {
                        DriverError::ConfigurationError(format!(
                            "Expected boolean-compatible value for single command: {e}"
                        ))
                    })?;
                    let info = SingleCommandInfo::new(p.ioa, b, false);
                    let session_cloned = Arc::clone(&session);
                    Self::spawn_session_with_timeout(timeout_ms, async move {
                        session_cloned.single_cmd(type_id, cot, ca, info).await
                    })
                    .await
                }
                TypeID::C_DC_NA_1 | TypeID::C_DC_TA_1 => {
                    let v: u8 = (&value).try_into().map_err(|e: NGValueCastError| {
                        DriverError::ConfigurationError(format!(
                            "Expected numeric value for double command: {e}"
                        ))
                    })?;
                    let info = DoubleCommandInfo::new(p.ioa, v, false);
                    let session_cloned = Arc::clone(&session);
                    Self::spawn_session_with_timeout(timeout_ms, async move {
                        session_cloned.double_cmd(type_id, cot, ca, info).await
                    })
                    .await
                }
                TypeID::C_RC_NA_1 | TypeID::C_RC_TA_1 => {
                    let v: u8 = (&value).try_into().map_err(|e: NGValueCastError| {
                        DriverError::ConfigurationError(format!(
                            "Expected numeric value for step command: {e}"
                        ))
                    })?;
                    let info = StepCommandInfo::new(p.ioa, v, false);
                    let session_cloned = Arc::clone(&session);
                    Self::spawn_session_with_timeout(timeout_ms, async move {
                        session_cloned.step_cmd(type_id, cot, ca, info).await
                    })
                    .await
                }
                TypeID::C_SE_NA_1 | TypeID::C_SE_TA_1 => {
                    let v: i16 = (&value).try_into().map_err(|e: NGValueCastError| {
                        DriverError::ConfigurationError(format!(
                            "Expected numeric value for set point command normal: {e}"
                        ))
                    })?;
                    let info = SetPointCommandNormalInfo::new(p.ioa, v);
                    let session_cloned = Arc::clone(&session);
                    Self::spawn_session_with_timeout(timeout_ms, async move {
                        session_cloned
                            .set_point_cmd_normal(type_id, cot, ca, info)
                            .await
                    })
                    .await
                }
                TypeID::C_SE_NB_1 | TypeID::C_SE_TB_1 => {
                    let v: i16 = (&value).try_into().map_err(|e: NGValueCastError| {
                        DriverError::ConfigurationError(format!(
                            "Expected numeric value for set point command scaled: {e}"
                        ))
                    })?;
                    let info = SetPointCommandScaledInfo::new(p.ioa, v);
                    let session_cloned = Arc::clone(&session);
                    Self::spawn_session_with_timeout(timeout_ms, async move {
                        session_cloned
                            .set_point_cmd_scaled(type_id, cot, ca, info)
                            .await
                    })
                    .await
                }
                TypeID::C_SE_NC_1 | TypeID::C_SE_TC_1 => {
                    let v: f32 = (&value).try_into().map_err(|e: NGValueCastError| {
                        DriverError::ConfigurationError(format!(
                            "Expected numeric value for set point command float: {e}"
                        ))
                    })?;
                    let info = SetPointCommandFloatInfo::new(p.ioa, v);
                    let session_cloned = Arc::clone(&session);
                    Self::spawn_session_with_timeout(timeout_ms, async move {
                        session_cloned
                            .set_point_cmd_float(type_id, cot, ca, info)
                            .await
                    })
                    .await
                }
                TypeID::C_BO_NA_1 | TypeID::C_BO_TA_1 => {
                    let v: i32 = (&value).try_into().map_err(|e: NGValueCastError| {
                        DriverError::ConfigurationError(format!(
                            "Expected numeric value for bits string 32 command: {e}"
                        ))
                    })?;
                    let info = BitsString32CommandInfo::new(p.ioa, v);
                    let session_cloned = Arc::clone(&session);
                    Self::spawn_session_with_timeout(timeout_ms, async move {
                        session_cloned
                            .bits_string32_cmd(type_id, cot, ca, info)
                            .await
                    })
                    .await
                }
                // Interrogation commands can be modeled as actions with no value; use config defaults
                TypeID::C_IC_NA_1 => {
                    // General Interrogation
                    let qoi = ObjectQOI::new(self.inner.config.startup_qoi);
                    let session_cloned = Arc::clone(&session);
                    Self::spawn_session_with_timeout(timeout_ms, async move {
                        session_cloned.interrogation_cmd(cot, ca, qoi).await
                    })
                    .await
                }
                TypeID::C_CI_NA_1 => {
                    // Counter Interrogation
                    let qcc = ObjectQCC::new(self.inner.config.startup_qcc);
                    let session_cloned = Arc::clone(&session);
                    Self::spawn_session_with_timeout(timeout_ms, async move {
                        session_cloned.counter_interrogation_cmd(cot, ca, qcc).await
                    })
                    .await
                }
                other => {
                    return Err(DriverError::ConfigurationError(format!(
                        "Unsupported IEC104 write type: {:?}",
                        other
                    )));
                }
            };

            // Update metrics
            self.total_requests.fetch_add(1, Ordering::Relaxed);
            let elapsed_ms = start_ts.elapsed().as_millis() as u64;
            match fut_res {
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
                Err(e) => {
                    self.failed_requests.fetch_add(1, Ordering::Relaxed);
                    return Err(e);
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
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let device =
            device
                .downcast_ref::<Iec104Device>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not Iec104Device for IEC104".to_string(),
                ))?;
        let point = point
            .downcast_ref::<Iec104Point>()
            .ok_or(DriverError::ConfigurationError(
                "RuntimePoint is not Iec104Point for IEC104".to_string(),
            ))?;

        if !matches!(point.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            return Err(DriverError::ValidationError(
                "point is not writeable".to_string(),
            ));
        }

        let point_type_id = TypeID::try_from(point.type_id).map_err(|_| {
            DriverError::ConfigurationError(format!("Unknown IEC104 type id: {}", point.type_id))
        })?;
        let cmd_type_id = point_type_id
            .map_to_command()
            .ok_or(DriverError::ConfigurationError(format!(
                "IEC104 point type {:?} is not supported for write_point",
                point_type_id
            )))?;

        let effective_timeout_ms = timeout_ms
            .unwrap_or(self.inner.connection_policy.write_timeout_ms)
            .max(1);

        let session = self
            .session
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)?;

        // Common constants
        let ca: u16 = device.ca;
        let cot = CauseOfTransmission::new(false, false, Cause::Activation);
        let time_opt = if cmd_type_id.needs_time() {
            Some(Utc::now())
        } else {
            None
        };

        // Conversion policy is centralized in `ng_gateway_sdk::NGValue` via `TryFrom/TryInto`.
        // Driver code should not re-implement numeric casting rules.

        let start_ts = Instant::now();
        let fut_res: DriverResult<()> = match cmd_type_id {
            TypeID::C_SC_NA_1 | TypeID::C_SC_TA_1 => {
                let b: bool = value.try_into().map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!("single command expects bool: {e}"))
                })?;
                let mut info = SingleCommandInfo::new(point.ioa, b, false);
                info.time = time_opt;
                let session_cloned = Arc::clone(&session);
                Self::spawn_session_with_timeout(effective_timeout_ms, async move {
                    session_cloned.single_cmd(cmd_type_id, cot, ca, info).await
                })
                .await
            }
            TypeID::C_DC_NA_1 | TypeID::C_DC_TA_1 => {
                let v: u8 = value.try_into().map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!("double command expects u8: {e}"))
                })?;
                let mut info = DoubleCommandInfo::new(point.ioa, v, false);
                info.time = time_opt;
                let session_cloned = Arc::clone(&session);
                Self::spawn_session_with_timeout(effective_timeout_ms, async move {
                    session_cloned.double_cmd(cmd_type_id, cot, ca, info).await
                })
                .await
            }
            TypeID::C_RC_NA_1 | TypeID::C_RC_TA_1 => {
                let v: u8 = value.try_into().map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!("step command expects u8: {e}"))
                })?;
                let mut info = StepCommandInfo::new(point.ioa, v, false);
                info.time = time_opt;
                let session_cloned = Arc::clone(&session);
                Self::spawn_session_with_timeout(effective_timeout_ms, async move {
                    session_cloned.step_cmd(cmd_type_id, cot, ca, info).await
                })
                .await
            }
            TypeID::C_SE_NA_1 | TypeID::C_SE_TA_1 => {
                let v: i16 = value.try_into().map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "set point (normal) expects i16-compatible value: {e}"
                    ))
                })?;
                let mut info = SetPointCommandNormalInfo::new(point.ioa, v);
                info.time = time_opt;
                let session_cloned = Arc::clone(&session);
                Self::spawn_session_with_timeout(effective_timeout_ms, async move {
                    session_cloned
                        .set_point_cmd_normal(cmd_type_id, cot, ca, info)
                        .await
                })
                .await
            }
            TypeID::C_SE_NB_1 | TypeID::C_SE_TB_1 => {
                let v: i16 = value.try_into().map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "set point (scaled) expects i16-compatible value: {e}"
                    ))
                })?;
                let mut info = SetPointCommandScaledInfo::new(point.ioa, v);
                info.time = time_opt;
                let session_cloned = Arc::clone(&session);
                Self::spawn_session_with_timeout(effective_timeout_ms, async move {
                    session_cloned
                        .set_point_cmd_scaled(cmd_type_id, cot, ca, info)
                        .await
                })
                .await
            }
            TypeID::C_SE_NC_1 | TypeID::C_SE_TC_1 => {
                let v: f32 = value.try_into().map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "set point (float) expects f32-compatible value: {e}"
                    ))
                })?;
                let mut info = SetPointCommandFloatInfo::new(point.ioa, v);
                info.time = time_opt;
                let session_cloned = Arc::clone(&session);
                Self::spawn_session_with_timeout(effective_timeout_ms, async move {
                    session_cloned
                        .set_point_cmd_float(cmd_type_id, cot, ca, info)
                        .await
                })
                .await
            }
            TypeID::C_BO_NA_1 | TypeID::C_BO_TA_1 => {
                let v: i32 = value.try_into().map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "bits string 32 command expects i32-compatible value: {e}"
                    ))
                })?;
                let mut info = BitsString32CommandInfo::new(point.ioa, v);
                info.time = time_opt;
                let session_cloned = Arc::clone(&session);
                Self::spawn_session_with_timeout(effective_timeout_ms, async move {
                    session_cloned
                        .bits_string32_cmd(cmd_type_id, cot, ca, info)
                        .await
                })
                .await
            }
            other => Err(DriverError::ConfigurationError(format!(
                "Unsupported IEC104 write command type: {:?}",
                other
            ))),
        };

        // Metrics (align with execute_action)
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        let elapsed_ms = start_ts.elapsed().as_millis() as u64;
        match fut_res {
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
            Err(e) => {
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
                return Err(e);
            }
        }

        Ok(WriteResult {
            outcome: WriteOutcome::Applied,
            applied_value: Some(value.clone()),
        })
    }

    #[inline]
    async fn apply_runtime_delta(&self, delta: RuntimeDelta) -> DriverResult<()> {
        match delta {
            RuntimeDelta::DevicesChanged {
                added,
                updated,
                removed,
                status_changed: _,
            } => {
                // Collect distinct CAs for interrogation commands
                let mut cas = HashSet::new();

                // For added/updated devices: points will be updated via PointsChanged delta
                // We just collect CAs for interrogation
                for d in added
                    .iter()
                    .chain(updated.iter())
                    .filter_map(|d| d.downcast_ref::<Iec104Device>())
                {
                    cas.insert(d.ca);
                }

                // Remove devices: remove all points belonging to these devices from their CA snapshots
                for d in removed
                    .iter()
                    .filter_map(|d| d.downcast_ref::<Iec104Device>())
                {
                    let ca = d.ca;
                    let device_id = d.id;

                    if let Some(snapshot_swap) = self.ca_to_snapshot.get(&ca) {
                        // Check if snapshot will be empty after removal
                        let current_snapshot = snapshot_swap.load();
                        let will_be_empty = current_snapshot
                            .points
                            .values()
                            .all(|meta| meta.device_id == device_id);

                        if will_be_empty {
                            // Remove the entire CA entry if no points remain
                            self.ca_to_snapshot.remove(&ca);
                        } else {
                            // Otherwise, update snapshot to remove this device's points
                            snapshot_swap.rcu(|snap| {
                                let mut new_snap = snap.as_ref().clone();
                                // Remove all points belonging to this device
                                new_snap
                                    .points
                                    .retain(|_, meta| meta.device_id != device_id);
                                // Remove id_index entries for removed points
                                new_snap
                                    .id_index
                                    .retain(|_, k| new_snap.points.contains_key(k));
                                new_snap
                            });
                        }
                    }
                    cas.insert(ca);
                }

                // Trigger interrogation commands for affected CAs
                if !cas.is_empty() {
                    if let Some(session) = self.session.load_full() {
                        let cot = CauseOfTransmission::new(false, false, Cause::Activation);
                        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);

                        for ca in cas {
                            if self.inner.config.auto_startup_general_interrogation {
                                let qoi = ObjectQOI::new(self.inner.config.startup_qoi);
                                let session_cloned = Arc::clone(&session);
                                let _ = Self::spawn_session_with_timeout(timeout_ms, async move {
                                    session_cloned.interrogation_cmd(cot, ca, qoi).await
                                })
                                .await;
                            }

                            if self.inner.config.auto_startup_counter_interrogation {
                                let qcc = ObjectQCC::new(self.inner.config.startup_qcc);
                                let session_cloned = Arc::clone(&session);
                                let _ = Self::spawn_session_with_timeout(timeout_ms, async move {
                                    session_cloned.counter_interrogation_cmd(cot, ca, qcc).await
                                })
                                .await;
                            }
                        }
                    }
                }
            }
            RuntimeDelta::PointsChanged {
                device,
                added,
                updated,
                removed,
            } => {
                let (ca, device_id, device_name) =
                    if let Some(dev) = device.downcast_ref::<Iec104Device>() {
                        (dev.ca, dev.id, Arc::<str>::from(dev.device_name.clone()))
                    } else {
                        return Ok(());
                    };

                // Get or create snapshot for this CA
                let snapshot_swap = self
                    .ca_to_snapshot
                    .entry(ca)
                    .or_insert(ArcSwap::from_pointee(CaSnapshot {
                        points: HashMap::new(),
                        id_index: HashMap::new(),
                    }));

                snapshot_swap.rcu(|snap| {
                  let mut new_snap = snap.as_ref().clone();

                  // Add new points
                  for p in added.iter().filter_map(|p| p.downcast_ref::<Iec104Point>()) {
                      let k = pack_key(p.ioa, p.type_id);
                      // Warn if duplicate (type_id, ioa) found for different devices
                      if let Some(existing) = new_snap.points.get(&k) {
                          if existing.device_id != device_id {
                              tracing::warn!(
                                  "Duplicate (type_id={}, ioa={}) found for CA {}: device {} vs device {}, keeping first",
                                  p.type_id, p.ioa, ca, existing.device_id, device_id
                              );
                              continue; // Keep first, skip duplicate
                          }
                      }
                      new_snap.points.insert(
                          k,
                          PointMeta {
                              point_id: p.id(),
                              device_id,
                              device_name: device_name.clone(),
                              point_key: Arc::<str>::from(p.key()),
                              data_type: p.wire_data_type(),
                              transform: p.transform,
                              kind: p.r#type,
                          },
                      );
                      new_snap.id_index.insert(p.id(), k);
                  }

                  // Update existing points
                  for p in updated
                      .iter()
                      .filter_map(|p| p.downcast_ref::<Iec104Point>())
                  {
                      let new_k = pack_key(p.ioa, p.type_id);
                      if let Some(old_k) = new_snap.id_index.get(&p.id()).copied() {
                          if old_k != new_k {
                              new_snap.points.remove(&old_k);
                          }
                      }
                      // Warn if duplicate (type_id, ioa) found for different devices
                      if let Some(existing) = new_snap.points.get(&new_k) {
                          if existing.device_id != device_id {
                              tracing::warn!(
                                  "Duplicate (type_id={}, ioa={}) found for CA {}: device {} vs device {}, keeping first",
                                  p.type_id, p.ioa, ca, existing.device_id, device_id
                              );
                              continue; // Keep first, skip duplicate
                          }
                      }
                      new_snap.points.insert(
                          new_k,
                          PointMeta {
                              point_id: p.id(),
                              device_id,
                              device_name: device_name.clone(),
                              point_key: Arc::<str>::from(p.key()),
                              data_type: p.wire_data_type(),
                              transform: p.transform,
                              kind: p.r#type,
                          },
                      );
                      new_snap.id_index.insert(p.id(), new_k);
                  }

                  // Remove points
                  for p in removed
                      .iter()
                      .filter_map(|p| p.downcast_ref::<Iec104Point>())
                  {
                      new_snap.id_index.remove(&p.id());
                      let k = pack_key(p.ioa, p.type_id);
                      // Only remove if it belongs to this device
                      if let Some(meta) = new_snap.points.get(&k) {
                          if meta.device_id == device_id {
                              new_snap.points.remove(&k);
                          }
                      }
                  }

                  new_snap
              });

                // Trigger interrogation commands when points are added or updated
                // to fetch initial/current values from the remote station
                if !added.is_empty() || !updated.is_empty() {
                    // Ensure session available
                    if let Some(session) = self.session.load_full() {
                        // Read channel config
                        let cot = CauseOfTransmission::new(false, false, Cause::Activation);
                        let timeout_ms = self.inner.connection_policy.write_timeout_ms.max(1);

                        if self.inner.config.auto_startup_general_interrogation {
                            let qoi = ObjectQOI::new(self.inner.config.startup_qoi);
                            let session_cloned = Arc::clone(&session);
                            let _ = Self::spawn_session_with_timeout(timeout_ms, async move {
                                session_cloned.interrogation_cmd(cot, ca, qoi).await
                            })
                            .await;
                        }

                        if self.inner.config.auto_startup_counter_interrogation {
                            let qcc = ObjectQCC::new(self.inner.config.startup_qcc);
                            let session_cloned = Arc::clone(&session);
                            let _ = Self::spawn_session_with_timeout(timeout_ms, async move {
                                session_cloned.counter_interrogation_cmd(cot, ca, qcc).await
                            })
                            .await;
                        }
                    }
                }
            }
            RuntimeDelta::ActionsChanged { .. } => {}
        }
        Ok(())
    }
}
