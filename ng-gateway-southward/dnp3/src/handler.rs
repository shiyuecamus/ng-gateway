use crate::{
    codec::Dnp3Codec,
    types::{Dnp3PointGroup, PointMeta},
};
use dashmap::DashMap;
use dnp3::{
    app::{
        measurement::{
            AnalogInput, AnalogOutputStatus, BinaryInput, BinaryOutputStatus, Counter, DoubleBit,
            DoubleBitBinaryInput, FrozenCounter,
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
        f: F,
    ) where
        F: FnOnce(&PointMeta) -> Option<NGValue>,
    {
        if let Some(meta) = points_map.get(&(group, index)) {
            if let Some(value) = f(&meta) {
                match meta.kind {
                    DataPointType::Telemetry => {
                        let entry = telemetry_buffer
                            .entry(meta.device_id)
                            .or_insert_with(|| (Arc::clone(&meta.device_name), Vec::new()));
                        entry.1.push(PointValue {
                            point_id: meta.point_id,
                            point_key: Arc::clone(&meta.key),
                            value,
                        });
                    }
                    DataPointType::Attribute => {
                        let entry = attribute_buffer
                            .entry(meta.device_id)
                            .or_insert_with(|| (Arc::clone(&meta.device_name), Vec::new()));
                        entry.1.push(PointValue {
                            point_id: meta.point_id,
                            point_key: Arc::clone(&meta.key),
                            value,
                        });
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
        for (device_id, (device_name, values)) in self.telemetry_buffer.drain() {
            if !values.is_empty() {
                let data = NorthwardData::Telemetry(TelemetryData::new(
                    device_id,
                    device_name.as_ref(),
                    values,
                ));
                let _ = self.publisher.try_publish(Arc::new(data));
            }
        }
        for (device_id, (device_name, values)) in self.attribute_buffer.drain() {
            if !values.is_empty() {
                let data = NorthwardData::Attributes(AttributeData::new_client_attributes(
                    device_id,
                    device_name.as_ref(),
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
        // Group 1
        for (value, index) in iter {
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::BinaryInput,
                index,
                |meta| Dnp3Codec::bool_to_value(value.value, meta),
            );
        }
    }

    fn handle_double_bit_binary_input(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (DoubleBitBinaryInput, u16)>,
    ) {
        // Group 3
        for (value, index) in iter {
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
                |meta| Dnp3Codec::u64_to_value(v as u64, meta),
            );
        }
    }

    fn handle_binary_output_status(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (BinaryOutputStatus, u16)>,
    ) {
        // Group 10
        for (value, index) in iter {
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::BinaryOutput,
                index,
                |meta| Dnp3Codec::bool_to_value(value.value, meta),
            );
        }
    }

    fn handle_counter(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (Counter, u16)>,
    ) {
        // Group 20
        for (value, index) in iter {
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::Counter,
                index,
                |meta| Dnp3Codec::u64_to_value(value.value as u64, meta),
            );
        }
    }

    fn handle_frozen_counter(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (FrozenCounter, u16)>,
    ) {
        // Group 21
        for (value, index) in iter {
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::FrozenCounter,
                index,
                |meta| Dnp3Codec::u64_to_value(value.value as u64, meta),
            );
        }
    }

    fn handle_analog_input(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (AnalogInput, u16)>,
    ) {
        // Group 30
        for (value, index) in iter {
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::AnalogInput,
                index,
                |meta| Dnp3Codec::f64_to_value(value.value, meta),
            );
        }
    }

    fn handle_analog_output_status(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (AnalogOutputStatus, u16)>,
    ) {
        // Group 40
        for (value, index) in iter {
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::AnalogOutput,
                index,
                |meta| Dnp3Codec::f64_to_value(value.value, meta),
            );
        }
    }

    fn handle_octet_string<'a>(
        &mut self,
        _info: HeaderInfo,
        iter: &'a mut dyn Iterator<Item = (&'a [u8], u16)>,
    ) {
        // Group 110 (Static) or 111 (Event)
        // Note: DNP3 library doesn't distinguish group in this callback args,
        // but typically OctetString is mapped to 110/111.
        // We can use 110 as generic "Octet String" group for mapping.
        for (value, index) in iter {
            Self::buffer_with_meta_lookup(
                &self.points_map,
                &mut self.telemetry_buffer,
                &mut self.attribute_buffer,
                Dnp3PointGroup::OctetString,
                index,
                |meta| Dnp3Codec::octets_to_value(value, meta),
            );
        }
    }
}
