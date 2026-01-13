use super::{
    codec::ModbusCodec,
    types::{Endianness, ModbusFunctionCode, ModbusParameter, ModbusPoint, ReadBatch, WritePlan},
};
use ng_gateway_sdk::{DriverError, DriverResult, NGValue};
use std::{collections::HashMap, sync::Arc};

/// Configuration for ModbusPlanner planning behavior.
///
/// This keeps the planner pure and decoupled from I/O or session concerns.
/// Only soft limits defined by driver configuration are used here; no protocol hard limits.
#[derive(Debug, Clone, Copy)]
pub struct ModbusPlannerConfig {
    /// Maximum address gap (inclusive) allowed when coalescing adjacent points.
    pub max_gap: u16,
    /// Maximum quantity (span) allowed per merged batch.
    pub max_batch: u16,
}

/// High-performance Modbus request planner for batching reads.
///
/// The planner groups points by function code, sorts by address, and coalesces adjacent
/// (or within-gap) points into range reads, while respecting the configured maximum span.
/// It performs no network I/O and allocates minimally on hot paths.
pub struct ModbusPlanner;

impl ModbusPlanner {
    /// Plan read batches by function code with max_gap/max_batch constraints.
    ///
    /// Input points are filtered to Read/ReadWrite and grouped by function code.
    /// Within each group, points are sorted by address and coalesced into contiguous
    /// ranges if the start-to-end span does not exceed `max_batch` and the inter-point
    /// gap does not exceed `max_gap`.
    pub fn plan_read_batches<'a>(
        cfg: ModbusPlannerConfig,
        points: &[&'a ModbusPoint],
    ) -> Vec<ReadBatch<'a>> {
        if points.is_empty() {
            return Vec::new();
        }

        // Group by function code (as u8 to avoid Hash bound on enum)
        let mut groups = HashMap::<u8, Vec<&'a ModbusPoint>>::with_capacity(8);
        for p in points {
            groups.entry(p.function_code.into()).or_default().push(*p);
        }

        let mut batches = Vec::with_capacity(points.len() / 4 + 1);

        for (_fc_u8, mut vecp) in groups {
            // Sort by address ascending
            vecp.sort_by_key(|p| p.address);
            let mut i = 0usize;
            while i < vecp.len() {
                let first = vecp[i];
                let batch_start = first.address;
                let mut batch_end = first.address.saturating_add(first.quantity.max(1) - 1);
                let mut batch_points = Vec::with_capacity(8);
                batch_points.push(first);
                i += 1;

                while i < vecp.len() {
                    let next = vecp[i];
                    let next_start = next.address;
                    let next_end = next.address.saturating_add(next.quantity.max(1) - 1);

                    let span_if_merged = next_end.saturating_sub(batch_start) + 1;
                    let gap_ok = next_start.saturating_sub(batch_end) <= cfg.max_gap;

                    if gap_ok && span_if_merged <= cfg.max_batch {
                        batch_points.push(next);
                        batch_end = batch_end.max(next_end);
                        i += 1;
                    } else {
                        break;
                    }
                }

                // function code is identical for all points in this group; read from the first
                let function = batch_points
                    .first()
                    .map(|p| p.function_code)
                    .unwrap_or_else(|| vecp[0].function_code);
                batches.push(ReadBatch {
                    function,
                    start_addr: batch_start,
                    quantity: batch_end.saturating_sub(batch_start) + 1,
                    points: batch_points,
                });
            }
        }

        batches
    }

    /// Plan write operations from resolved parameters into protocol-specific write plans.
    ///
    /// This function encodes payloads using ModbusCodec based on the parameter data type
    /// and the provided byte and word order, and returns a list of low-level `WritePlan`s
    /// ready to be executed by the driver.
    pub fn plan_write_plans(
        items: &[(Arc<ModbusParameter>, NGValue)],
        byte_order: Endianness,
        word_order: Endianness,
    ) -> DriverResult<Vec<WritePlan>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        let mut plans: Vec<WritePlan> = Vec::with_capacity(items.len().max(1));

        for (mp, value) in items.iter() {
            match mp.function_code {
                ModbusFunctionCode::WriteSingleCoil => {
                    let coils = ModbusCodec::encode_coils(value, Some(1))?;
                    plans.push(WritePlan {
                        function: mp.function_code,
                        address: mp.address,
                        coils: Some(coils),
                        registers: None,
                    });
                }
                ModbusFunctionCode::WriteMultipleCoils => {
                    let coils = ModbusCodec::encode_coils(value, Some(mp.quantity as usize))?;
                    plans.push(WritePlan {
                        function: mp.function_code,
                        address: mp.address,
                        coils: Some(coils),
                        registers: None,
                    });
                }
                ModbusFunctionCode::WriteSingleRegister => {
                    let mut regs = ModbusCodec::encode_registers_from_value(
                        value,
                        mp.data_type,
                        byte_order,
                        word_order,
                    )?;
                    if regs.is_empty() {
                        return Err(DriverError::PlannerError(
                            "Encoded register payload is empty".to_string(),
                        ));
                    }
                    regs.truncate(1);
                    plans.push(WritePlan {
                        function: mp.function_code,
                        address: mp.address,
                        coils: None,
                        registers: Some(regs),
                    });
                }
                ModbusFunctionCode::WriteMultipleRegisters => {
                    let registers = ModbusCodec::encode_registers_from_value(
                        value,
                        mp.data_type,
                        byte_order,
                        word_order,
                    )?;
                    plans.push(WritePlan {
                        function: mp.function_code,
                        address: mp.address,
                        coils: None,
                        registers: Some(registers),
                    });
                }
                other => {
                    return Err(DriverError::ConfigurationError(format!(
                        "Unsupported function for write action: {:?}",
                        other
                    )));
                }
            }
        }

        Ok(plans)
    }
}
