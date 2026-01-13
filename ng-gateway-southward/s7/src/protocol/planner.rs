use super::{
    error::{Error, Result},
    frame::{
        decode_datetime8, decode_dtl12, latin1_bytes_to_string, s5time_to_duration,
        s7_date_to_naive_date, s7_tod_to_naive_time, S7DataValue, S7ReturnCode, S7TransportSize,
        S7VarSpec, VarPayloadDataItemIter,
    },
};
use bytes::{Bytes, BytesMut};
use chrono::Duration;
use nom::number::complete::{be_u16, u8 as nom_u8};
use std::{
    cmp::{max, min, Ordering},
    collections::{HashMap, HashSet},
    mem::take,
};

// Shared S7 protocol constants used by planner and capacity helpers
const S7_REQ_HEADER_JOB: usize = 10; // S7 Job header length (no error code)
const S7_RESP_HEADER_ACK_DATA: usize = 12; // S7 AckData header length (includes error code)
const S7_REQ_PARAM_BASE: usize = 2; // function + item_count
const S7_RESP_PARAM_BASE: usize = 2; // function + item_count
const S7_VAR_SPEC_LEN: usize = 12; // per item VarSpec length in ReadVar/WriteVar param

/// High-performance S7 request planner for batching, splitting and merging.
///
/// This module contains pure planning logic which:
/// - Packs read/write items into batches under the negotiated S7 PDU length
/// - Splits over-sized read items into multiple sub-requests
/// - Provides helpers to merge read responses back to original order
///
/// It operates only at the S7 PDU level (param_len + payload_len) and deliberately ignores
/// transport segmentation details (handled by session/codec layers).
#[derive(Debug, Clone, Copy)]
pub struct PlannerConfig {
    /// Negotiated S7 PDU length (header + param_len + payload_len). This is the FULL S7 PDU.
    pub s7_pdu_len: u16,
    /// Optional: allow filling up to N bytes gap between adjacent items when coalescing.
    /// This only applies to non-Bit items and only when the gap is aligned to element size.
    /// None disables gap fill, Some(0) is equivalent to strict adjacency.
    pub gap_bytes: Option<usize>,
    /// Optional: maximum number of items allowed per ReadVar/WriteVar request.
    /// If None, no explicit max-item constraint is applied (capacity still enforced).
    pub max_items_per_request: Option<usize>,
    /// Optional: penalty bytes applied when mixing different (area, db, transport_size)
    /// tuples within the same batch. The penalty is added once per area switch decision
    /// during read packing, effectively encouraging homogeneous batches without strictly
    /// forbidding mixing. None disables the penalty.
    pub area_switch_penalty_bytes: Option<usize>,
}

impl PlannerConfig {
    pub fn new(s7_pdu_len: u16) -> Self {
        Self {
            s7_pdu_len,
            gap_bytes: None,
            max_items_per_request: None,
            area_switch_penalty_bytes: None,
        }
    }

    /// Configure maximum gap bytes allowed during coalescing. Alignment to element size required.
    #[inline]
    pub fn with_coalesce_gap_bytes(mut self, gap: Option<usize>) -> Self {
        self.gap_bytes = gap;
        self
    }

    /// Configure maximum items per request.
    #[inline]
    pub fn with_max_items_per_request(mut self, max_items: Option<usize>) -> Self {
        self.max_items_per_request = max_items;
        self
    }

    /// Configure area switch penalty in bytes for read batching.
    #[inline]
    pub fn with_area_switch_penalty_bytes(mut self, penalty: Option<usize>) -> Self {
        self.area_switch_penalty_bytes = penalty;
        self
    }
}

/// Key identifying a logical S7 memory region granularity suitable for batching decisions.
///
/// This aggregates the area, DB number and transport size. It is used to
/// compare, group and apply penalties when switching regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct AreaKey {
    /// S7 memory area encoded as u8
    area: u8,
    /// DB number (0 for non-DB areas)
    db: u16,
    /// Transport size encoded as u8
    ts: u8,
}

impl From<&S7VarSpec> for AreaKey {
    #[inline]
    fn from(s: &S7VarSpec) -> Self {
        Self {
            area: s.area as u8,
            db: s.db_number,
            ts: s.transport_size as u8,
        }
    }
}

/// Result item of a merged read: per original spec
#[derive(Debug, Clone)]
pub struct ReadItemMerged {
    /// Response code after merging fragments; any non-OK in fragments yields that code
    pub rc: S7ReturnCode,
    /// Merged data bytes in original order
    pub data: Bytes,
}

/// Merged read result (per original spec order)
pub type ReadMerged = Vec<ReadItemMerged>;

/// Zero-copy merged view per original item: a sequence of borrowed slices back into response PDUs.
#[derive(Debug, Clone)]
pub struct ReadItemView<'a> {
    /// Response code after merging fragments; any non-OK in fragments yields that code
    pub rc: S7ReturnCode,
    /// Borrowed slices that together form the original item's data in order
    pub slices: Vec<&'a [u8]>,
}

/// Zero-copy view for an entire merge result
pub type ReadMergedView<'a> = Vec<ReadItemView<'a>>;

/// Result item of a typed merged read: per original spec
#[derive(Debug, Clone)]
pub struct ReadItemTyped {
    /// Response code after merging fragments
    pub rc: S7ReturnCode,
    /// Decoded typed value
    pub value: Option<S7DataValue>,
}

/// Typed merged read result (per original spec order)
pub type ReadMergedTyped = Vec<ReadItemTyped>;

/// Internal descriptor of a planned fragment for merging
#[derive(Debug, Clone, Copy)]
struct FragmentRef {
    batch_index: usize,
    item_index_in_batch: usize,
    /// Offset in bytes in the merged result buffer where this fragment starts
    merge_offset: usize,
    /// Expected fragment data length in bytes
    fragment_len: usize,
    /// Source offset in bytes within the batch item data to copy from
    source_offset: usize,
}

/// A planned read consisting of batches of var specs plus merge mapping
#[derive(Debug, Clone)]
pub struct ReadPlan {
    pub batches: Vec<Vec<S7VarSpec>>, // per-batch ReadVar specs
    // For each original item index -> list of fragment refs in order
    per_item_fragments: Vec<Vec<FragmentRef>>,
    // For each original item, pre-computed total size and element size (bytes)
    per_item_total_len: Vec<usize>,
    // Original specs in the same order as caller provided to plan_read
    orig_specs: Vec<S7VarSpec>,
}

/// A planned write consisting of batches of (spec, data)
#[derive(Debug, Clone)]
pub struct WritePlan {
    pub batches: Vec<Vec<(S7VarSpec, Bytes)>>,
}

// Build pending fragments either via coalescing or 1:1 items
#[derive(Debug, Clone)]
struct RefPiece {
    orig_index: usize,
    merge_offset: usize,
    source_offset: usize,
    len: usize,
}
#[derive(Debug, Clone)]
struct PendingFrag {
    spec: S7VarSpec,
    elem_size: usize,
    refs: Vec<RefPiece>,
}

/// High-level planner entrypoints
pub struct S7Planner;

impl S7Planner {
    /// Plan read requests into batches and (if needed) split large items.
    pub fn plan_read(config: &PlannerConfig, specs: &[S7VarSpec]) -> ReadPlan {
        // Capacity model for READ:
        // We must ensure the RESPONSE fits in negotiated full PDU length:
        // total_len = S7Header(AckData) + resp_param_len + resp_payload_len <= s7_pdu_len
        // where:
        // - S7Header(AckData) is 12 bytes (includes 2-byte error code present)
        // - resp_param_len = 2 (function + item_count)
        // - resp_payload_len = sum(items: 1(rc)+1(type)+2(len)+data+pad)
        let full_cap = config.s7_pdu_len as usize;

        // Prepare outputs
        let mut batches: Vec<Vec<S7VarSpec>> = Vec::new();
        let mut cur_batch: Vec<S7VarSpec> = Vec::new();
        let mut cur_req_param_len: usize = S7_REQ_PARAM_BASE;
        let mut cur_resp_param_len: usize = S7_RESP_PARAM_BASE;
        let mut cur_resp_payload_len: usize = 0;

        // Merge mapping accumulators
        let mut per_item_fragments: Vec<Vec<FragmentRef>> = vec![Vec::new(); specs.len()];
        let mut per_item_total_len: Vec<usize> = Vec::with_capacity(specs.len());
        for s in specs.iter().copied() {
            let elem = s.transport_size.element_bytes();
            per_item_total_len.push(s.count as usize * elem);
        }

        // Step 0: group by (area, db, transport_size) and sort within group by start address
        // We maintain original indices for depack mapping.
        let mut ordered: Vec<(usize, S7VarSpec)> = specs.iter().copied().enumerate().collect();
        ordered.sort_by(|a, b| {
            let (_ia, sa) = a;
            let (_ib, sb) = b;
            let ka = AreaKey::from(sa);
            let kb = AreaKey::from(sb);
            match ka.cmp(&kb) {
                Ordering::Equal => {
                    // order by start bit-position: byte_address then bit_index
                    match sa.byte_address.cmp(&sb.byte_address) {
                        Ordering::Equal => sa.bit_index.cmp(&sb.bit_index),
                        o => o,
                    }
                }
                o => o,
            }
        });

        // Coalesce adjacent addresses into larger logical reads
        let mut pending: Vec<PendingFrag> = Vec::new();

        // Build groups over the ordered view
        let mut i = 0usize;
        while i < ordered.len() {
            let (first_idx, first) = ordered[i];
            let elem = first.transport_size.element_bytes();
            let mut total_elems: usize = first.count as usize;
            let mut parts: Vec<(usize, usize)> = Vec::new(); // (orig_index, elems)
            parts.push((first_idx, first.count as usize));
            let mut last_byte_addr = first.byte_address + (first.count as u32) * (elem as u32);
            let mut j = i + 1;
            while j < ordered.len() {
                let (s_idx, s) = ordered[j];
                // Fast reject if not compatible to be coalesced into the same group
                if !is_group_compatible(&s, &first) {
                    break;
                }
                // Determine if acceptable as adjacent or within gap (aligned)
                let gap_elems =
                    match compute_gap_elems(last_byte_addr, s.byte_address, elem, config.gap_bytes)
                    {
                        Some(g) => g,
                        None => break,
                    };
                let add = s.count as usize;
                if total_elems + gap_elems + add > u16::MAX as usize {
                    break;
                }
                parts.push((s_idx, add));
                total_elems += gap_elems + add;
                // advance last_byte_addr to the end of this item
                // If there was a positive gap, the merged group will include that gap implicitly
                // when we later compute sub-fragment byte addresses; data mapping keeps correctness.
                last_byte_addr = s.byte_address.saturating_add((add as u32) * (elem as u32));
                j += 1;
            }

            // Materialize this group as one pending logical fragment (will split below if needed)
            // If parts.len()==1, this degenerates to single item
            let mut group_spec = first;
            group_spec.count = total_elems as u16;
            group_spec.byte_address = first.byte_address;
            // Preserve bit_index for Bit transport size; reset to 0 for byte-addressed types.
            group_spec.bit_index = if matches!(group_spec.transport_size, S7TransportSize::Bit) {
                first.bit_index
            } else {
                0
            };

            // Now split this group if it cannot fit in one response payload
            // rc(1)+type(1)+len(2); base bound for a single item regardless of elem
            let per_item_overhead = 4;
            let max_payload_for_single = full_cap
                .saturating_sub(S7_RESP_HEADER_ACK_DATA)
                .saturating_sub(S7_RESP_PARAM_BASE)
                .saturating_sub(per_item_overhead);
            let max_elems = max(1, max_payload_for_single / elem);

            // Build a vector of part ranges in elems [start, len], considering possible gaps.
            let part_starts = build_part_starts(&parts, specs, &first, elem);

            // Accumulate per original item write offsets across all fragments of this group
            let mut item_merge_offsets: HashMap<usize, usize> = HashMap::new();
            for (orig_index, _elen) in parts.iter().copied() {
                item_merge_offsets.entry(orig_index).or_insert(0);
            }

            let mut left_total = total_elems;
            let mut group_elem_offset = 0usize;
            while left_total > 0 {
                let chunk_elems = min(left_total, max_elems);
                let mut sub = group_spec;
                sub.count = chunk_elems as u16;
                sub.byte_address = group_spec
                    .byte_address
                    .saturating_add((group_elem_offset * elem) as u32);

                // Build ref pieces intersecting this fragment
                let frag_start = group_elem_offset;
                let frag_end = group_elem_offset + chunk_elems;
                let refs = build_refs_for_fragment(
                    frag_start,
                    frag_end,
                    &parts,
                    &part_starts,
                    elem,
                    &mut item_merge_offsets,
                );

                pending.push(PendingFrag {
                    spec: sub,
                    elem_size: elem,
                    refs,
                });

                left_total -= chunk_elems;
                group_elem_offset += chunk_elems;
            }

            i = j;
        }

        // Place pending fragments into batches respecting both request and response limits
        // Additionally apply optional heuristics: max_items_per_request and area switch penalty.
        let mut last_area_key: Option<AreaKey> = None;
        let mut cur_items_in_batch: usize = 0; // number of fragments in batch
                                               // Track unique original item indices contained in current batch for max-items heuristic
        let mut cur_orig_item_set: HashSet<usize> = HashSet::with_capacity(32);
        let max_items = config.max_items_per_request.unwrap_or(usize::MAX);

        for pf in pending.into_iter() {
            let elem_size = pf.elem_size;
            let add_req_param = S7_VAR_SPEC_LEN;
            let frag_data = (pf.spec.count as usize) * elem_size;
            let area_key = AreaKey::from(&pf.spec);

            let penalty = compute_area_switch_penalty(
                config.area_switch_penalty_bytes,
                last_area_key,
                area_key,
                cur_items_in_batch,
            );
            let base_add_resp_payload = compute_add_resp_payload_read(frag_data, 0);
            let add_resp_payload_with_penalty = compute_add_resp_payload_read(frag_data, penalty);

            let place_new_batch = should_start_new_batch_read(
                full_cap,
                cur_req_param_len,
                cur_resp_param_len,
                cur_resp_payload_len,
                add_req_param,
                add_resp_payload_with_penalty,
                &cur_orig_item_set,
                &pf.refs,
                max_items,
            );

            if place_new_batch {
                start_new_batch_accumulate_read(
                    &mut batches,
                    &mut cur_batch,
                    &mut cur_req_param_len,
                    &mut cur_resp_param_len,
                    &mut cur_resp_payload_len,
                    &mut cur_items_in_batch,
                    &mut cur_orig_item_set,
                    base_add_resp_payload,
                );
            } else {
                cur_req_param_len += add_req_param;
                cur_resp_payload_len += add_resp_payload_with_penalty;
            }

            // fill fragment mapping for all original parts
            let item_index_in_batch = cur_batch.len();
            accumulate_fragment_mapping_read(
                batches.len(),
                item_index_in_batch,
                &pf.refs,
                &mut per_item_fragments,
                &mut cur_orig_item_set,
            );

            cur_batch.push(pf.spec);
            cur_items_in_batch += 1;
            last_area_key = Some(area_key);
        }

        if !cur_batch.is_empty() {
            batches.push(cur_batch);
        }

        ReadPlan {
            batches,
            per_item_fragments,
            per_item_total_len,
            orig_specs: specs.to_vec(),
        }
    }

    /// Plan write requests into batches. No splitting across a single (spec,data) item here,
    /// as write splitting semantics are ambiguous across types. If a single item does not fit
    /// the negotiated PDU length, the caller should pre-chunk data accordingly.
    pub fn plan_write(config: &PlannerConfig, items: &[(S7VarSpec, Bytes)]) -> WritePlan {
        // Capacity model for WRITE:
        // We must ensure the REQUEST fits in negotiated full PDU length:
        // total_len = S7Header(Job) + req_param_len + req_payload_len <= s7_pdu_len
        // where header(Job) is 10 bytes (error_code absent)
        // req_param_len = 2 + N*12
        // req_payload_len = sum(items: 1+1+2+data+pad)
        // Additionally check response capacity: 12 + 2 + N*1 <= s7_pdu_len
        let full_cap = config.s7_pdu_len as usize;

        let mut batches: Vec<Vec<(S7VarSpec, Bytes)>> = Vec::new();
        let mut cur_batch: Vec<(S7VarSpec, Bytes)> = Vec::new();
        let mut cur_req_param_len: usize = S7_REQ_PARAM_BASE;
        let mut cur_req_payload_len: usize = 0;
        let mut cur_resp_param_len: usize = S7_RESP_PARAM_BASE;
        let mut cur_resp_payload_len: usize = 0; // N items: 1 byte each aggregated

        let max_items = config.max_items_per_request.unwrap_or(usize::MAX);
        let mut cur_items_in_batch: usize = 0;
        for (spec, data) in items.iter().cloned() {
            let add_req_param = S7_VAR_SPEC_LEN;
            let add_req_payload = 1 + 1 + 2 + data.len() + pad_byte(data.len());
            let add_resp_payload = 1; // one rc byte

            let next_req_param_len = cur_req_param_len + add_req_param;
            let next_req_payload_len = cur_req_payload_len + add_req_payload;
            let next_resp_payload_len = cur_resp_payload_len + add_resp_payload;

            let ok = fits_write_cap(
                full_cap,
                next_req_param_len,
                next_req_payload_len,
                cur_resp_param_len,
                next_resp_payload_len,
            );

            if ok && cur_items_in_batch < max_items {
                cur_batch.push((spec, data));
                cur_req_param_len = next_req_param_len;
                cur_req_payload_len = next_req_payload_len;
                cur_resp_payload_len = next_resp_payload_len;
                cur_items_in_batch += 1;
            } else {
                if !cur_batch.is_empty() {
                    batches.push(cur_batch);
                }
                cur_batch = Vec::new();
                cur_req_param_len = S7_REQ_PARAM_BASE + S7_VAR_SPEC_LEN;
                cur_req_payload_len = add_req_payload;
                cur_resp_param_len = S7_RESP_PARAM_BASE;
                cur_resp_payload_len = add_resp_payload;
                cur_batch.push((spec, data));
                cur_items_in_batch = 1;
            }
        }

        if !cur_batch.is_empty() {
            batches.push(cur_batch);
        }

        WritePlan { batches }
    }
}

#[inline]
fn is_group_compatible(s: &S7VarSpec, first: &S7VarSpec) -> bool {
    if s.area != first.area
        || s.db_number != first.db_number
        || s.transport_size != first.transport_size
    {
        return false;
    }

    match s.transport_size {
        S7TransportSize::Bit => {
            s.byte_address == first.byte_address && s.bit_index == first.bit_index
        }
        _ => s.bit_index == 0 && first.bit_index == 0,
    }
}

#[inline]
fn compute_gap_elems(
    last_byte_addr: u32,
    next_byte_addr: u32,
    elem_size: usize,
    gap_bytes_cfg: Option<usize>,
) -> Option<usize> {
    if next_byte_addr == last_byte_addr {
        return Some(0);
    }
    let max_gap = gap_bytes_cfg?;
    if next_byte_addr <= last_byte_addr {
        return None;
    }
    let gap = (next_byte_addr - last_byte_addr) as usize;
    if gap > max_gap || !gap.is_multiple_of(elem_size) {
        return None;
    }
    Some(gap / elem_size)
}

#[inline]
fn build_part_starts(
    parts: &[(usize, usize)],
    specs: &[S7VarSpec],
    first: &S7VarSpec,
    elem: usize,
) -> Vec<(usize, usize)> {
    let mut part_starts: Vec<(usize, usize)> = Vec::with_capacity(parts.len());
    let mut acc = 0usize;
    let mut prev_end_byte = first.byte_address;
    for (idx, (orig_index, elen)) in parts.iter().copied().enumerate() {
        let current_start_byte = if idx == 0 {
            first.byte_address
        } else {
            let s = specs[orig_index];
            s.byte_address
        };
        let gap_bytes = current_start_byte as isize - prev_end_byte as isize;
        if gap_bytes > 0 {
            let gap = gap_bytes as usize;
            let gap_elems = gap / elem;
            acc += gap_elems;
        }
        part_starts.push((acc, elen));
        acc += elen;
        prev_end_byte = current_start_byte.saturating_add((elen as u32) * (elem as u32));
    }
    part_starts
}

#[inline]
fn build_refs_for_fragment(
    frag_start: usize,
    frag_end: usize,
    parts: &[(usize, usize)],
    part_starts: &[(usize, usize)],
    elem: usize,
    item_merge_offsets: &mut HashMap<usize, usize>,
) -> Vec<RefPiece> {
    let mut refs: Vec<RefPiece> = Vec::new();
    for (k, (orig_index, _elen)) in parts.iter().copied().enumerate() {
        let (part_start, plen) = part_starts[k];
        let part_end = part_start + plen;
        let ov_start = max(frag_start, part_start);
        let ov_end = min(frag_end, part_end);
        if ov_start < ov_end {
            let ov_elems = ov_end - ov_start;
            let source_offset = (ov_start - frag_start) * elem;
            let len = ov_elems * elem;
            let entry = item_merge_offsets.entry(orig_index).or_insert(0);
            let merge_offset = *entry;
            *entry += len;
            refs.push(RefPiece {
                orig_index,
                merge_offset,
                source_offset,
                len,
            });
        }
    }
    refs
}

impl ReadPlan {
    /// Merge a sequence of ReadVar AckData PDUs into per-original-spec results.
    /// The `responses` slice must correspond 1:1 to `self.batches`, in the same order.
    pub fn merge(&self, responses: &[Bytes]) -> ReadMerged {
        // Build an index: for each batch, decode once into vector of (rc, data_slice)
        // We avoid copying here by borrowing slices from the `responses` buffers, and only
        // perform a single copy per original item into its final merged buffer.
        let mut per_batch_items = Vec::with_capacity(responses.len());
        for (batch_idx, raw) in responses.iter().enumerate() {
            // For merge we only need payload bytes area of AckData; however callers provide
            // already-sliced payload for simplicity. If provided is entire PDU, we expect
            // the slice here to be the raw payload region (param-decoupled). Upstream helpers
            // in session will extract the correct portion.
            // Here we parse item_count implicitly by iterating until failure is not possible,
            // so require the caller to pass (item_count, raw) to eliminate ambiguity.
            // To stay self-contained, we reconstruct item_count from planned batch.
            let item_count = self.batches.get(batch_idx).map(|v| v.len()).unwrap_or(0) as u8;
            let mut items = Vec::with_capacity(item_count as usize);
            for (it, _rest) in VarPayloadDataItemIter::new(item_count, raw.as_ref()) {
                items.push((it.return_code, it.data));
            }
            per_batch_items.push(items);
        }

        // Merge per original item using fragment mapping
        let mut merged = Vec::with_capacity(self.per_item_fragments.len());

        for (item_idx, fragments) in self.per_item_fragments.iter().enumerate() {
            let total = self.per_item_total_len[item_idx];
            let mut buf = BytesMut::with_capacity(total);
            // Pre-fill to correct length to allow in place copy by offset
            buf.resize(total, 0);

            let mut rc_final = S7ReturnCode::Success;
            for fr in fragments {
                let &(rc, data) = &per_batch_items[fr.batch_index][fr.item_index_in_batch];
                if !matches!(rc, S7ReturnCode::Success) {
                    rc_final = rc;
                }
                let target_range = fr.merge_offset..(fr.merge_offset + fr.fragment_len);
                if data.len() >= fr.source_offset + fr.fragment_len {
                    buf[target_range].copy_from_slice(
                        &data[fr.source_offset..fr.source_offset + fr.fragment_len],
                    );
                } else {
                    // Defensive: if device returned fewer bytes than planned, truncate
                    let available = data.len().saturating_sub(fr.source_offset);
                    let copy_len = min(available, fr.fragment_len);
                    if copy_len > 0 {
                        buf[fr.merge_offset..fr.merge_offset + copy_len]
                            .copy_from_slice(&data[fr.source_offset..fr.source_offset + copy_len]);
                    }
                }
            }
            merged.push(ReadItemMerged {
                rc: rc_final,
                data: buf.freeze(),
            });
        }

        merged
    }

    /// Zero-copy view merging. Returns, for each original item, a list of slice views pointing
    /// into the provided `responses` buffers. The caller must ensure those buffers outlive the view.
    pub fn merge_view<'a>(&'a self, responses: &'a [Bytes]) -> ReadMergedView<'a> {
        // Decode once per batch into (rc, data_slice) borrowing from `responses`
        let mut per_batch_items = Vec::with_capacity(responses.len());
        for (batch_idx, raw) in responses.iter().enumerate() {
            let item_count = self.batches.get(batch_idx).map(|v| v.len()).unwrap_or(0) as u8;
            let mut items = Vec::with_capacity(item_count as usize);
            for (it, _rest) in VarPayloadDataItemIter::new(item_count, raw.as_ref()) {
                items.push((it.return_code, it.data));
            }
            per_batch_items.push(items);
        }

        // Build views per original item
        let mut merged = Vec::with_capacity(self.per_item_fragments.len());
        for fragments in self.per_item_fragments.iter() {
            let mut rc_final = S7ReturnCode::Success;
            let mut slices = Vec::with_capacity(fragments.len());
            for fr in fragments {
                let &(rc, data) = &per_batch_items[fr.batch_index][fr.item_index_in_batch];
                if !matches!(rc, S7ReturnCode::Success) {
                    rc_final = rc;
                }
                let start = fr.source_offset;
                let end = fr.source_offset.saturating_add(fr.fragment_len);
                if start <= data.len() {
                    let end = min(end, data.len());
                    let slice = &data[start..end];
                    if !slice.is_empty() {
                        slices.push(slice);
                    }
                }
            }
            merged.push(ReadItemView {
                rc: rc_final,
                slices,
            });
        }
        merged
    }

    /// Merge and decode typed values using a single contiguous buffer per original item.
    /// This uses the same per-batch item parsing path as `merge`, then for each original
    /// item it copies fragments once into a contiguous buffer and decodes it according
    /// to its original `S7VarSpec` using nom-based parsers.
    pub fn merge_typed(&self, responses: &[Bytes]) -> Result<ReadMergedTyped> {
        // Step 1: per-batch decode -> Vec<Vec<(rc, data_slice)>>
        let mut per_batch_items = Vec::with_capacity(responses.len());
        for (batch_idx, raw) in responses.iter().enumerate() {
            let item_count = self.batches.get(batch_idx).map(|v| v.len()).unwrap_or(0) as u8;
            let mut items = Vec::with_capacity(item_count as usize);
            for (it, _rest) in VarPayloadDataItemIter::new(item_count, raw.as_ref()) {
                items.push((it.return_code, it.data));
            }
            per_batch_items.push(items);
        }

        // Step 2: per original item -> copy fragments into contiguous buffer and decode
        let mut out: ReadMergedTyped = Vec::with_capacity(self.per_item_fragments.len());
        for (item_idx, fragments) in self.per_item_fragments.iter().enumerate() {
            let total = self.per_item_total_len[item_idx];
            let mut buf = BytesMut::with_capacity(total);
            buf.resize(total, 0);
            let mut actual_len: usize = 0;

            let mut rc_final = S7ReturnCode::Success;
            for fr in fragments {
                let &(rc, data) = &per_batch_items[fr.batch_index][fr.item_index_in_batch];
                if !matches!(rc, S7ReturnCode::Success) {
                    rc_final = rc;
                }
                let (copy_len, dst_off) = if data.len() >= fr.source_offset + fr.fragment_len {
                    (fr.fragment_len, fr.merge_offset)
                } else {
                    let available = data.len().saturating_sub(fr.source_offset);
                    let copy_len = min(available, fr.fragment_len);
                    (copy_len, fr.merge_offset)
                };
                if copy_len > 0 {
                    buf[dst_off..dst_off + copy_len]
                        .copy_from_slice(&data[fr.source_offset..fr.source_offset + copy_len]);
                    let end_pos = dst_off + copy_len;
                    if end_pos > actual_len {
                        actual_len = end_pos;
                    }
                }
            }

            if !matches!(rc_final, S7ReturnCode::Success) {
                out.push(ReadItemTyped {
                    rc: rc_final,
                    value: None,
                });
                continue;
            }

            // Decode according to original spec. If decode fails due to insufficient or
            // malformed data for this item, downgrade it to a per-item data-type error
            // instead of failing the entire batch, so callers still get results for
            // other addresses.
            let spec = self.orig_specs.get(item_idx).ok_or(Error::Decode {
                context: "missing original spec for item",
            })?;
            let slice = &buf[..actual_len.min(buf.len())];
            match decode_typed_scalar(spec, slice) {
                Ok(value) => {
                    out.push(ReadItemTyped {
                        rc: rc_final,
                        value: Some(value),
                    });
                }
                Err(Error::InsufficientData { .. }) | Err(Error::Decode { .. }) => {
                    // Represent per-item decode issues as a logical S7 data-type
                    // inconsistency instead of a hard error.
                    out.push(ReadItemTyped {
                        rc: S7ReturnCode::DataTypeInconsistent,
                        value: None,
                    });
                }
                Err(e) => return Err(e),
            }
        }

        Ok(out)
    }
}

#[inline]
fn compute_area_switch_penalty(
    penalty_cfg: Option<usize>,
    last_area_key: Option<AreaKey>,
    area_key: AreaKey,
    cur_items_in_batch: usize,
) -> usize {
    match (penalty_cfg, last_area_key) {
        (Some(p), Some(prev)) if prev != area_key && cur_items_in_batch > 0 => p,
        _ => 0,
    }
}

#[inline]
fn compute_add_resp_payload_read(frag_data_len: usize, extra_penalty: usize) -> usize {
    let per_item_overhead = 4; // rc(1)+type(1)+len(2)
    per_item_overhead + frag_data_len + pad_byte(frag_data_len) + extra_penalty
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn should_start_new_batch_read(
    full_cap: usize,
    cur_req_param_len: usize,
    cur_resp_param_len: usize,
    cur_resp_payload_len: usize,
    add_req_param: usize,
    add_resp_payload: usize,
    cur_orig_item_set: &HashSet<usize>,
    refs_to_add: &[RefPiece],
    max_items: usize,
) -> bool {
    if would_exceed_max_items(cur_orig_item_set, refs_to_add, max_items) {
        return true;
    }
    let next_req_param_len = cur_req_param_len + add_req_param;
    let next_resp_param_len = cur_resp_param_len; // stays 2
    let next_resp_payload_len = cur_resp_payload_len + add_resp_payload;
    !fits_read_cap(
        full_cap,
        next_req_param_len,
        next_resp_param_len,
        next_resp_payload_len,
    )
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn start_new_batch_accumulate_read(
    batches: &mut Vec<Vec<S7VarSpec>>,
    cur_batch: &mut Vec<S7VarSpec>,
    cur_req_param_len: &mut usize,
    cur_resp_param_len: &mut usize,
    cur_resp_payload_len: &mut usize,
    cur_items_in_batch: &mut usize,
    cur_orig_item_set: &mut HashSet<usize>,
    base_add_resp_payload: usize,
) {
    if !cur_batch.is_empty() {
        batches.push(take(cur_batch));
    }
    *cur_req_param_len = S7_REQ_PARAM_BASE + S7_VAR_SPEC_LEN;
    *cur_resp_param_len = S7_RESP_PARAM_BASE;
    *cur_resp_payload_len = base_add_resp_payload; // penalty applies only on decision boundary
    *cur_items_in_batch = 0;
    cur_orig_item_set.clear();
}

#[inline]
fn accumulate_fragment_mapping_read(
    batch_index: usize,
    item_index_in_batch: usize,
    refs: &[RefPiece],
    per_item_fragments: &mut [Vec<FragmentRef>],
    cur_orig_item_set: &mut HashSet<usize>,
) {
    for r in refs.iter() {
        per_item_fragments[r.orig_index].push(FragmentRef {
            batch_index,
            item_index_in_batch,
            merge_offset: r.merge_offset,
            fragment_len: r.len,
            source_offset: r.source_offset,
        });
        cur_orig_item_set.insert(r.orig_index);
    }
}

#[inline]
fn decode_typed_scalar(spec: &S7VarSpec, data: &[u8]) -> Result<S7DataValue> {
    match spec.transport_size {
        S7TransportSize::Bit => {
            if data.is_empty() {
                return Err(Error::InsufficientData {
                    needed: 1,
                    available: data.len(),
                });
            }
            let bit = (data[0] & 0x01) != 0;
            Ok(S7DataValue::Bit(bit))
        }
        S7TransportSize::Byte => {
            let v = *data.first().ok_or(Error::InsufficientData {
                needed: 1,
                available: data.len(),
            })?;
            Ok(S7DataValue::Byte(v))
        }
        S7TransportSize::Char => {
            let b = *data.first().ok_or(Error::InsufficientData {
                needed: 1,
                available: data.len(),
            })?;
            Ok(S7DataValue::Char(b as char))
        }
        S7TransportSize::Word => {
            if data.len() < 2 {
                return Err(Error::InsufficientData {
                    needed: 2,
                    available: data.len(),
                });
            }
            let v = u16::from_be_bytes([data[0], data[1]]);
            Ok(S7DataValue::Word(v))
        }
        S7TransportSize::Int => {
            if data.len() < 2 {
                return Err(Error::InsufficientData {
                    needed: 2,
                    available: data.len(),
                });
            }
            let v = i16::from_be_bytes([data[0], data[1]]);
            Ok(S7DataValue::Int(v))
        }
        S7TransportSize::DWord => {
            if data.len() < 4 {
                return Err(Error::InsufficientData {
                    needed: 4,
                    available: data.len(),
                });
            }
            let v = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            Ok(S7DataValue::DWord(v))
        }
        S7TransportSize::DInt => {
            if data.len() < 4 {
                return Err(Error::InsufficientData {
                    needed: 4,
                    available: data.len(),
                });
            }
            let v = i32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            Ok(S7DataValue::DInt(v))
        }
        S7TransportSize::Real => {
            if data.len() < 4 {
                return Err(Error::InsufficientData {
                    needed: 4,
                    available: data.len(),
                });
            }
            let bits = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            Ok(S7DataValue::Real(f32::from_bits(bits)))
        }
        S7TransportSize::Date => {
            if data.len() < 2 {
                return Err(Error::InsufficientData {
                    needed: 2,
                    available: data.len(),
                });
            }
            let days = u16::from_be_bytes([data[0], data[1]]);
            let d = s7_date_to_naive_date(days).ok_or(Error::Decode {
                context: "invalid S7 Date",
            })?;
            Ok(S7DataValue::Date(d))
        }
        S7TransportSize::TimeOfDay => {
            if data.len() < 4 {
                return Err(Error::InsufficientData {
                    needed: 4,
                    available: data.len(),
                });
            }
            let ms = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            let t = s7_tod_to_naive_time(ms).ok_or(Error::Decode {
                context: "invalid TimeOfDay",
            })?;
            Ok(S7DataValue::TimeOfDay(t))
        }
        S7TransportSize::Time => {
            if data.len() < 4 {
                return Err(Error::InsufficientData {
                    needed: 4,
                    available: data.len(),
                });
            }
            let ms = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            Ok(S7DataValue::Time(Duration::milliseconds(ms as i64)))
        }
        S7TransportSize::S5Time => {
            if data.len() < 2 {
                return Err(Error::InsufficientData {
                    needed: 2,
                    available: data.len(),
                });
            }
            let raw = u16::from_be_bytes([data[0], data[1]]);
            Ok(S7DataValue::S5Time(s5time_to_duration(raw)))
        }
        S7TransportSize::Counter => {
            if data.len() < 2 {
                return Err(Error::InsufficientData {
                    needed: 2,
                    available: data.len(),
                });
            }
            let v = i16::from_be_bytes([data[0], data[1]]) as i32;
            Ok(S7DataValue::Counter(v))
        }
        S7TransportSize::Timer => {
            if data.len() < 2 {
                return Err(Error::InsufficientData {
                    needed: 2,
                    available: data.len(),
                });
            }
            let raw = u16::from_be_bytes([data[0], data[1]]);
            Ok(S7DataValue::Timer(s5time_to_duration(raw)))
        }
        S7TransportSize::DateTime => {
            if data.len() < 8 {
                return Err(Error::InsufficientData {
                    needed: 8,
                    available: data.len(),
                });
            }
            let dt = decode_datetime8(&data[0..8]).ok_or(Error::Decode {
                context: "invalid S7 DateTime",
            })?;
            Ok(S7DataValue::DateTime(dt))
        }
        S7TransportSize::DateTimeLong => {
            if data.len() < 12 {
                return Err(Error::InsufficientData {
                    needed: 12,
                    available: data.len(),
                });
            }
            let dt = decode_dtl12(&data[0..12]).ok_or(Error::Decode {
                context: "invalid DTL",
            })?;
            Ok(S7DataValue::DateTimeLong(dt))
        }
        S7TransportSize::String => {
            // Parse one or more S7 String(s): [max: u8][len: u8][payload: len]
            // Only count=1 supported
            let (i2, _max) =
                nom_u8::<_, nom::error::Error<&[u8]>>(data).map_err(|_| Error::Decode {
                    context: "string header max",
                })?;
            let (i3, len) =
                nom_u8::<_, nom::error::Error<&[u8]>>(i2).map_err(|_| Error::Decode {
                    context: "string header len",
                })?;
            if i3.len() < len as usize {
                return Err(Error::InsufficientData {
                    needed: len as usize,
                    available: i3.len(),
                });
            }
            let (_rest, payload) =
                nom::bytes::complete::take::<_, _, nom::error::Error<&[u8]>>(len as usize)(i3)
                    .map_err(|_| Error::Decode {
                        context: "string payload",
                    })?;
            let s = latin1_bytes_to_string(payload);
            Ok(S7DataValue::String(s))
        }
        S7TransportSize::WString => {
            let (i2, _max) =
                be_u16::<_, nom::error::Error<&[u8]>>(data).map_err(|_| Error::Decode {
                    context: "wstring header max",
                })?;
            let (i3, len) =
                be_u16::<_, nom::error::Error<&[u8]>>(i2).map_err(|_| Error::Decode {
                    context: "wstring header len",
                })?;
            let bytes_needed = (len as usize) * 2;
            if i3.len() < bytes_needed {
                return Err(Error::InsufficientData {
                    needed: bytes_needed,
                    available: i3.len(),
                });
            }
            let (_rest, payload) =
                nom::bytes::complete::take::<_, _, nom::error::Error<&[u8]>>(bytes_needed)(i3)
                    .map_err(|_| Error::Decode {
                        context: "wstring payload",
                    })?;
            let mut units: Vec<u16> = Vec::with_capacity(len as usize);
            for j in 0..(len as usize) {
                let o = j * 2;
                units.push(u16::from_be_bytes([payload[o], payload[o + 1]]));
            }
            let s = String::from_utf16(&units).map_err(|_| Error::Decode {
                context: "invalid UTF-16 in WString",
            })?;
            Ok(S7DataValue::WString(s))
        }
        // Not expected here; reserved/other sizes treated as unsupported for typed decode
        other => {
            let _ = other;
            Err(Error::UnsupportedFeature {
                feature: "typed decode for transport size",
            })
        }
    }
}

#[inline]
fn pad_byte(n: usize) -> usize {
    n & 1
}

/// Evaluate if a read batch fits capacity after adding one item.
///
/// This checks:
/// - Response: 12 + resp_param_len + resp_payload_len <= full_cap
/// - Request: 10 + req_param_len <= full_cap (read has no request payload)
#[inline]
fn fits_read_cap(
    full_cap: usize,
    next_req_param_len: usize,
    next_resp_param_len: usize,
    next_resp_payload_len: usize,
) -> bool {
    (S7_RESP_HEADER_ACK_DATA + next_resp_param_len + next_resp_payload_len <= full_cap)
        && (S7_REQ_HEADER_JOB + next_req_param_len <= full_cap)
}

/// Evaluate if a write batch fits capacity after adding one item.
///
/// This checks:
/// - Request: 10 + req_param_len + req_payload_len <= full_cap
/// - Response: 12 + resp_param_len + resp_payload_len <= full_cap
#[inline]
fn fits_write_cap(
    full_cap: usize,
    next_req_param_len: usize,
    next_req_payload_len: usize,
    next_resp_param_len: usize,
    next_resp_payload_len: usize,
) -> bool {
    (S7_REQ_HEADER_JOB + next_req_param_len + next_req_payload_len <= full_cap)
        && (S7_RESP_HEADER_ACK_DATA + next_resp_param_len + next_resp_payload_len <= full_cap)
}

/// Check if adding `refs_to_add` would exceed the configured maximum number of
/// unique original items per request. This counts unique original indices, not
/// fragment count, which is usually more intuitive for callers.
#[inline]
fn would_exceed_max_items(
    cur_orig_item_set: &HashSet<usize>,
    refs_to_add: &[RefPiece],
    max_items: usize,
) -> bool {
    if cur_orig_item_set.len() >= max_items {
        return true;
    }
    // Fast path: if every ref belongs to already present items, never exceeds
    // Otherwise, count distinct new indices and compare.
    let mut add_unique_orig = 0usize;
    for r in refs_to_add.iter() {
        if !cur_orig_item_set.contains(&r.orig_index) {
            add_unique_orig += 1;
            if cur_orig_item_set.len() + add_unique_orig > max_items {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::frame::types::{S7Area, S7TransportSize};
    use super::*;

    fn spec(ts: S7TransportSize, count: u16, db: u16, byte_addr: u32, bit: u8) -> S7VarSpec {
        S7VarSpec {
            transport_size: ts,
            count,
            db_number: db,
            area: S7Area::DB,
            byte_address: byte_addr,
            bit_index: bit,
        }
    }

    #[test]
    fn test_max_items_limit_splits_batches() {
        // Place items in different DBs so they don't coalesce into one var spec
        let a = spec(S7TransportSize::Byte, 4, 1, 0, 0);
        let b = spec(S7TransportSize::Byte, 4, 2, 0, 0);
        let c = spec(S7TransportSize::Byte, 4, 3, 0, 0);
        // Large PDU so capacity won't limit, but max_items=2 will force split
        let cfg = PlannerConfig::new(1024)
            .with_coalesce_gap_bytes(Some(0))
            .with_max_items_per_request(Some(2));
        let plan = S7Planner::plan_read(&cfg, &[a, b, c]);
        // Expect 2 batches: first with 2 items, second with 1 item
        assert_eq!(plan.batches.len(), 2);
        assert_eq!(plan.batches[0].len(), 2);
        assert_eq!(plan.batches[1].len(), 1);
    }

    #[test]
    fn test_area_switch_penalty_increases_batch_count() {
        // Craft two different DB groups to trigger penalty
        let a = spec(S7TransportSize::Byte, 8, 1, 0, 0);
        let b = spec(S7TransportSize::Byte, 8, 2, 0, 0);
        // Allow both to fit without penalty
        let base_cfg = PlannerConfig::new(128).with_coalesce_gap_bytes(Some(0));
        let base_plan = S7Planner::plan_read(&base_cfg, &[a, b]);
        // Add large penalty to force split into two batches
        let pen_cfg = PlannerConfig::new(128)
            .with_coalesce_gap_bytes(Some(0))
            .with_area_switch_penalty_bytes(Some(128));
        let pen_plan = S7Planner::plan_read(&pen_cfg, &[a, b]);
        assert!(base_plan.batches.len() <= pen_plan.batches.len());
        assert!(pen_plan.batches.len() >= 2);
    }

    #[test]
    fn test_bit_index_preserved_in_group_spec_and_coalesce_rule() {
        // Two Bit items at different bit positions must NOT coalesce unless bit_index equal.
        let a = S7VarSpec {
            transport_size: S7TransportSize::Bit,
            count: 1,
            db_number: 1,
            area: S7Area::DB,
            byte_address: 0,
            bit_index: 1,
        };
        let b = S7VarSpec {
            transport_size: S7TransportSize::Bit,
            count: 1,
            db_number: 1,
            area: S7Area::DB,
            byte_address: 0,
            bit_index: 3,
        };
        let cfg = PlannerConfig::new(256).with_coalesce_gap_bytes(Some(0));
        let plan = S7Planner::plan_read(&cfg, &[a, b]);
        // Bit items with different bit_index should be separate entries (no merge)
        let total_items: usize = plan.batches.iter().map(|v| v.len()).sum();
        assert_eq!(total_items, 2);
        // Ensure each preserves its bit_index
        for batch in plan.batches.iter() {
            for s in batch.iter() {
                assert!(matches!(s.transport_size, S7TransportSize::Bit));
                assert!(s.bit_index == 1 || s.bit_index == 3);
            }
        }
    }
}
