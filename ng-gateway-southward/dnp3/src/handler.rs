use crate::{
    codec::Dnp3Codec,
    types::{Dnp3PointGroup, PointMeta},
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use dnp3::{
    app::{
        measurement::{
            AnalogInput, AnalogOutputStatus, BinaryInput, BinaryOutputStatus, Counter, DoubleBit,
            DoubleBitBinaryInput, FrozenCounter, Time,
        },
        MaybeAsync, ResponseHeader,
    },
    master::{HeaderInfo, ReadHandler, ReadType},
};
use ng_gateway_sdk::{
    AttributeData, DataPointType, NGValue, NorthwardData, NorthwardPublisher, PointValue,
    TelemetryData,
};
use std::{collections::HashMap, sync::Arc};

/// Convert DNP3 `Time` to `chrono::DateTime<Utc>`.
///
/// DNP3 `Timestamp` wraps a u64 millisecond count since Unix epoch.
/// Both `Synchronized` and `Unsynchronized` variants carry the same
/// raw value; we discard the sync-quality flag here because northward
/// consumers care about the *value*, not the quality.
#[inline]
fn dnp3_time_to_chrono(time: &Time) -> Option<DateTime<Utc>> {
    time.timestamp().to_datetime_utc()
}

pub struct Dnp3SoeHandler {
    pub points_map: Arc<DashMap<(Dnp3PointGroup, u16), PointMeta>>,
    pub publisher: Arc<dyn NorthwardPublisher>,

    // device_id -> (device_name, values)
    telemetry_buffer: HashMap<i32, (Arc<str>, Vec<PointValue>)>,
    attribute_buffer: HashMap<i32, (Arc<str>, Vec<PointValue>)>,
}

impl Dnp3SoeHandler {
    pub fn new(
        points_map: Arc<DashMap<(Dnp3PointGroup, u16), PointMeta>>,
        publisher: Arc<dyn NorthwardPublisher>,
    ) -> Self {
        Self {
            points_map,
            publisher,
            telemetry_buffer: HashMap::new(),
            attribute_buffer: HashMap::new(),
        }
    }

    #[inline]
    fn buffer_with_meta_lookup<F>(
        points_map: &DashMap<(Dnp3PointGroup, u16), PointMeta>,
        telemetry_buffer: &mut HashMap<i32, (Arc<str>, Vec<PointValue>)>,
        attribute_buffer: &mut HashMap<i32, (Arc<str>, Vec<PointValue>)>,
        group: Dnp3PointGroup,
        index: u16,
        ts: Option<DateTime<Utc>>,
        f: F,
    ) where
        F: FnOnce(&PointMeta) -> Option<NGValue>,
    {
        if let Some(meta) = points_map.get(&(group, index)) {
            if let Some(value) = f(&meta) {
                let pv = PointValue {
                    point_id: meta.point_id,
                    point_key: Arc::clone(&meta.key),
                    value,
                    ts,
                };
                match meta.kind {
                    DataPointType::Telemetry => {
                        let entry = telemetry_buffer
                            .entry(meta.device_id)
                            .or_insert((Arc::clone(&meta.device_name), Vec::new()));
                        entry.1.push(pv);
                    }
                    DataPointType::Attribute => {
                        let entry = attribute_buffer
                            .entry(meta.device_id)
                            .or_insert((Arc::clone(&meta.device_name), Vec::new()));
                        entry.1.push(pv);
                    }
                }
            }
        }
    }
}

impl ReadHandler for Dnp3SoeHandler {
    fn begin_fragment(&mut self, _read_type: ReadType, _header: ResponseHeader) -> MaybeAsync<()> {
        self.telemetry_buffer.clear();
        self.attribute_buffer.clear();
        MaybeAsync::ready(())
    }

    fn end_fragment(&mut self, _read_type: ReadType, _header: ResponseHeader) -> MaybeAsync<()> {
        let now = Utc::now();
        for (device_id, (device_name, values)) in self.telemetry_buffer.drain() {
            if !values.is_empty() {
                let data = NorthwardData::Telemetry(TelemetryData::new_with_ts(
                    device_id,
                    device_name.as_ref(),
                    now,
                    values,
                ));
                let _ = self.publisher.try_publish(Arc::new(data));
            }
        }
        for (device_id, (device_name, values)) in self.attribute_buffer.drain() {
            if !values.is_empty() {
                let data = NorthwardData::Attributes(AttributeData::new_client_attributes_with_ts(
                    device_id,
                    device_name.as_ref(),
                    now,
                    values,
                ));
                let _ = self.publisher.try_publish(Arc::new(data));
            }
        }
        MaybeAsync::ready(())
    }

    fn handle_binary_input(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (BinaryInput, u16)>,
    ) {
        for (value, index) in iter {
            let ts = value.time.as_ref().and_then(dnp3_time_to_chrono);
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::BinaryInput,
                index,
                ts,
                |meta| Dnp3Codec::bool_to_value(value.value, meta),
            );
        }
    }

    fn handle_double_bit_binary_input(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (DoubleBitBinaryInput, u16)>,
    ) {
        for (value, index) in iter {
            let ts = value.time.as_ref().and_then(dnp3_time_to_chrono);
            let v = match value.value {
                DoubleBit::Intermediate => 0,
                DoubleBit::DeterminedOff => 1,
                DoubleBit::DeterminedOn => 2,
                DoubleBit::Indeterminate => 3,
            };
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::DoubleBitBinaryInput,
                index,
                ts,
                |meta| Dnp3Codec::u64_to_value(v as u64, meta),
            );
        }
    }

    fn handle_binary_output_status(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (BinaryOutputStatus, u16)>,
    ) {
        for (value, index) in iter {
            let ts = value.time.as_ref().and_then(dnp3_time_to_chrono);
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::BinaryOutput,
                index,
                ts,
                |meta| Dnp3Codec::bool_to_value(value.value, meta),
            );
        }
    }

    fn handle_counter(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (Counter, u16)>,
    ) {
        for (value, index) in iter {
            let ts = value.time.as_ref().and_then(dnp3_time_to_chrono);
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::Counter,
                index,
                ts,
                |meta| Dnp3Codec::u64_to_value(value.value as u64, meta),
            );
        }
    }

    fn handle_frozen_counter(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (FrozenCounter, u16)>,
    ) {
        for (value, index) in iter {
            let ts = value.time.as_ref().and_then(dnp3_time_to_chrono);
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::FrozenCounter,
                index,
                ts,
                |meta| Dnp3Codec::u64_to_value(value.value as u64, meta),
            );
        }
    }

    fn handle_analog_input(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (AnalogInput, u16)>,
    ) {
        for (value, index) in iter {
            let ts = value.time.as_ref().and_then(dnp3_time_to_chrono);
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::AnalogInput,
                index,
                ts,
                |meta| Dnp3Codec::f64_to_value(value.value, meta),
            );
        }
    }

    fn handle_analog_output_status(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (AnalogOutputStatus, u16)>,
    ) {
        for (value, index) in iter {
            let ts = value.time.as_ref().and_then(dnp3_time_to_chrono);
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::AnalogOutput,
                index,
                ts,
                |meta| Dnp3Codec::f64_to_value(value.value, meta),
            );
        }
    }

    fn handle_octet_string<'a>(
        &mut self,
        _info: HeaderInfo,
        iter: &'a mut dyn Iterator<Item = (&'a [u8], u16)>,
    ) {
        for (value, index) in iter {
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::OctetString,
                index,
                None,
                |meta| Dnp3Codec::octets_to_value(value, meta),
            );
        }
    }
}
