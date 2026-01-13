use super::{
    frame::{addr::McLogicalAddress, device::McDeviceType},
    types::McSeries,
};
use bytes::{BufMut, Bytes, BytesMut};

/// MC planner configuration derived from channel settings.
///
/// This type mirrors the existing driver batching behaviour and is used to
/// centralise the limits for both read and write paths.
#[derive(Debug, Clone, Copy)]
pub struct PlannerConfig {
    /// Maximum logical points per batch.
    pub max_points_per_batch: u16,
    /// Maximum payload bytes per frame.
    pub max_bytes_per_frame: u16,
}

impl PlannerConfig {
    pub fn new(max_points_per_batch: u16, max_bytes_per_frame: u16) -> Self {
        Self {
            max_points_per_batch,
            max_bytes_per_frame,
        }
    }
}

/// Logical read specification for a single MC item.
#[derive(Debug, Clone)]
pub struct PointReadSpec {
    /// Caller-supplied index used to keep a stable mapping between input order
    /// and merged read results.
    pub index: usize,
    /// Parsed logical address.
    pub addr: McLogicalAddress,
    /// Number of logical words for this point.
    pub word_len: u16,
    /// Encoded device code used by MC protocol.
    pub device_code: u16,
}

/// Entry within a read batch describing a point slice in the batch payload.
#[derive(Debug, Clone)]
pub struct BatchEntry {
    /// Index of the original spec in the caller-provided list.
    pub index: usize,
    /// Offset measured in logical words from the batch head.
    pub offset_words: u16,
    /// Length in logical words for this point.
    pub len_words: u16,
}

/// Concrete read batch specification matching the driver's existing encoding path.
#[derive(Debug, Clone)]
pub struct ReadBatchSpec {
    /// Device type for this batch.
    pub device: McDeviceType,
    /// Encoded device code used on the wire.
    pub device_code: u16,
    /// Head logical address (device index) of the batch.
    pub head: u32,
    /// Total logical points (words) within this batch.
    pub points: u16,
    /// Entries describing individual points within the batch.
    pub entries: Vec<BatchEntry>,
}

/// Logical write specification for a single MC parameter or point.
#[derive(Debug, Clone)]
pub struct WriteEntry {
    /// Parsed logical address.
    pub addr: McLogicalAddress,
    /// Number of logical words for this write.
    pub word_len: u16,
    /// Encoded device code used by MC protocol.
    pub device_code: u16,
    /// Fully encoded payload for this entry.
    pub data: Bytes,
}

/// Concrete write batch specification matching the driver's existing encoding path.
#[derive(Debug, Clone)]
pub struct WriteBatchSpec {
    /// Device type for this batch.
    pub device: McDeviceType,
    /// Encoded device code used on the wire.
    pub device_code: u16,
    /// Head logical address (device index) of the batch.
    pub head: u32,
    /// Total logical points (words) within this batch.
    pub points: u16,
    /// Contiguous payload buffer for all entries in the batch.
    pub data: BytesMut,
}

/// Logical random read specification for a single MC item.
///
/// This type is used by the random-read planner to group word/dword addresses
/// into MC `DeviceAccessRandomReadUnits` commands.
#[derive(Debug, Clone)]
pub struct RandomReadEntry {
    /// Index into the caller-provided logical item list.
    pub index: usize,
    /// Parsed logical address.
    pub addr: McLogicalAddress,
    /// Encoded device code used by MC protocol.
    pub device_code: u16,
}

/// Random-read batch specification describing a single MC random read command.
#[derive(Debug, Clone)]
pub struct RandomReadBatchSpec {
    /// Word-sized entries (16‑bit units).
    pub word_entries: Vec<RandomReadEntry>,
    /// Dword-sized entries (32‑bit units).
    pub dword_entries: Vec<RandomReadEntry>,
}

/// Logical random write specification for a single MC item.
///
/// This type is used by the random-write planner to group word/dword addresses
/// into MC `DeviceAccessRandomWriteUnits` commands.
#[allow(unused)]
#[derive(Debug, Clone)]
pub struct RandomWriteEntry {
    /// Parsed logical address.
    pub addr: McLogicalAddress,
    /// Encoded device code used by MC protocol.
    pub device_code: u16,
    /// Number of logical word units (16‑bit) covered by this write.
    pub word_len: u16,
    /// Fully encoded payload bytes for this entry.
    pub data: Bytes,
}

/// Random-write batch specification describing a single MC random write command.
#[derive(Debug, Clone)]
pub struct RandomWriteBatchSpec {
    /// Word-sized entries (16‑bit units).
    pub word_entries: Vec<RandomWriteEntry>,
    /// Dword-sized entries (32‑bit units).
    pub dword_entries: Vec<RandomWriteEntry>,
}

/// Raw read result item for MC protocol.
///
/// This type mirrors the S7 `ReadItemRaw` struct and carries per-item MC
/// completion code together with an optional payload slice. Higher layers are
/// responsible for interpreting the payload bytes according to their own
/// typing rules (for example, `DataType` + JSON in the `typed_api` facade).
#[derive(Debug, Clone)]
pub struct McReadItemRaw {
    /// Index into the original request list.
    pub index: usize,
    /// MC completion code for this item (0 means success).
    pub end_code: u16,
    /// Raw payload slice for this logical item when `end_code == 0`.
    ///
    /// The slice is represented as an owned `Bytes` value which usually points
    /// into a larger response buffer using cheap `Bytes::slice` views.
    pub payload: Option<Bytes>,
}

/// Per-batch result used by `merge_read_typed` to build item-level outputs.
#[derive(Debug, Clone)]
pub struct ReadBatchResult {
    /// MC completion code for this batch (0 means success).
    pub end_code: u16,
    /// Optional raw payload for this batch when `end_code == 0`.
    pub payload: Option<Bytes>,
}

/// Build read batches from point specs under planner capacity constraints.
///
/// This function preserves the original driver behaviour:
/// - specs are sorted by (device, head)
/// - batches are split when device changes, addresses are non-contiguous,
///   or capacity limits would be exceeded.
pub fn plan_read_batches(
    config: &PlannerConfig,
    mut specs: Vec<PointReadSpec>,
) -> Vec<ReadBatchSpec> {
    // Sort by device and head address to build contiguous batches.
    specs.sort_by_key(|s| (s.addr.device as u8, s.addr.head));

    let mut batches: Vec<ReadBatchSpec> = Vec::new();
    let mut cur: Option<ReadBatchSpec> = None;

    for spec in specs.into_iter() {
        let unit_size = spec.addr.device.unit_size();

        match &mut cur {
            None => {
                let mut entries = Vec::with_capacity(4);
                entries.push(BatchEntry {
                    index: spec.index,
                    offset_words: 0,
                    len_words: spec.word_len,
                });
                cur = Some(ReadBatchSpec {
                    device: spec.addr.device,
                    device_code: spec.device_code,
                    head: spec.addr.head,
                    points: spec.word_len,
                    entries,
                });
            }
            Some(batch) => {
                // If device type changes, flush current batch.
                if batch.device != spec.addr.device || batch.device_code != spec.device_code {
                    batches.push(ReadBatchSpec {
                        device: batch.device,
                        device_code: batch.device_code,
                        head: batch.head,
                        points: batch.points,
                        entries: batch.entries.clone(),
                    });
                    let mut entries = Vec::with_capacity(4);
                    entries.push(BatchEntry {
                        index: spec.index,
                        offset_words: 0,
                        len_words: spec.word_len,
                    });
                    *batch = ReadBatchSpec {
                        device: spec.addr.device,
                        device_code: spec.device_code,
                        head: spec.addr.head,
                        points: spec.word_len,
                        entries,
                    };
                    continue;
                }

                // Check address contiguity.
                let expected_head = batch.head.saturating_add(batch.points as u32);
                let contiguous = spec.addr.head == expected_head;

                let next_points = batch.points.saturating_add(spec.word_len);
                let next_bytes = (next_points as usize) * unit_size;
                let within_capacity = next_points as u32 <= config.max_points_per_batch as u32
                    && next_bytes as u32 <= config.max_bytes_per_frame as u32;

                if !contiguous || !within_capacity {
                    batches.push(ReadBatchSpec {
                        device: batch.device,
                        device_code: batch.device_code,
                        head: batch.head,
                        points: batch.points,
                        entries: batch.entries.clone(),
                    });
                    let mut entries = Vec::with_capacity(4);
                    entries.push(BatchEntry {
                        index: spec.index,
                        offset_words: 0,
                        len_words: spec.word_len,
                    });
                    *batch = ReadBatchSpec {
                        device: spec.addr.device,
                        device_code: spec.device_code,
                        head: spec.addr.head,
                        points: spec.word_len,
                        entries,
                    };
                } else {
                    let offset = batch.points;
                    batch.entries.push(BatchEntry {
                        index: spec.index,
                        offset_words: offset,
                        len_words: spec.word_len,
                    });
                    batch.points = next_points;
                }
            }
        }
    }

    if let Some(b) = cur {
        batches.push(b);
    }

    batches
}

/// Build write batches from logical write entries under planner capacity constraints.
///
/// This function preserves the original driver behaviour:
/// - entries are sorted by (device, head)
/// - batches are split when device changes, addresses are non-contiguous,
///   or capacity limits would be exceeded.
pub fn plan_write_batches(
    config: &PlannerConfig,
    mut entries: Vec<WriteEntry>,
) -> Vec<WriteBatchSpec> {
    // Sort by device and head address to build contiguous batches.
    entries.sort_by_key(|e| (e.addr.device as u8, e.addr.head));

    let mut batches: Vec<WriteBatchSpec> = Vec::new();
    let mut cur: Option<WriteBatchSpec> = None;

    for e in entries.into_iter() {
        let unit_size = e.addr.device.unit_size();
        let entry_bytes = (e.word_len as usize) * unit_size;

        match &mut cur {
            None => {
                let points = e.word_len;
                let mut data = BytesMut::with_capacity((points as usize) * unit_size);
                data.put_slice(e.data.as_ref());
                cur = Some(WriteBatchSpec {
                    device: e.addr.device,
                    device_code: e.device_code,
                    head: e.addr.head,
                    points,
                    data,
                });
            }
            Some(batch) => {
                if batch.device != e.addr.device || batch.device_code != e.device_code {
                    batches.push(WriteBatchSpec {
                        device: batch.device,
                        device_code: batch.device_code,
                        head: batch.head,
                        points: batch.points,
                        data: batch.data.clone(),
                    });
                    let points = e.word_len;
                    let mut data = BytesMut::with_capacity((points as usize) * unit_size);
                    data.put_slice(e.data.as_ref());
                    *batch = WriteBatchSpec {
                        device: e.addr.device,
                        device_code: e.device_code,
                        head: e.addr.head,
                        points,
                        data,
                    };
                    continue;
                }

                let expected_head = batch.head.saturating_add(batch.points as u32);
                let contiguous = e.addr.head == expected_head;

                let next_points = batch.points.saturating_add(e.word_len);
                let next_bytes = (next_points as usize) * unit_size;
                let within_capacity = next_points as u32 <= config.max_points_per_batch as u32
                    && next_bytes as u32 <= config.max_bytes_per_frame as u32;

                if !contiguous || !within_capacity {
                    batches.push(WriteBatchSpec {
                        device: batch.device,
                        device_code: batch.device_code,
                        head: batch.head,
                        points: batch.points,
                        data: batch.data.clone(),
                    });
                    let points = e.word_len;
                    let mut data = BytesMut::with_capacity((points as usize) * unit_size);
                    data.put_slice(e.data.as_ref());
                    *batch = WriteBatchSpec {
                        device: e.addr.device,
                        device_code: e.device_code,
                        head: e.addr.head,
                        points,
                        data,
                    };
                } else {
                    let offset_words = batch.points;
                    let offset_bytes = (offset_words as usize) * unit_size;
                    let new_total_bytes = next_bytes;
                    if batch.data.len() < new_total_bytes {
                        batch.data.resize(new_total_bytes, 0);
                    }
                    batch.data[offset_bytes..offset_bytes + entry_bytes]
                        .copy_from_slice(e.data.as_ref());
                    batch.points = next_points;
                }
            }
        }
    }

    if let Some(b) = cur {
        batches.push(b);
    }

    batches
}

/// Merge raw read results from batch payloads into per-item outputs.
///
/// This helper mirrors the S7 planner `merge_typed` behaviour:
/// - `batches` describes how logical items are packed into each batch payload.
/// - `batch_results` carries completion codes and optional payloads per batch.
///
/// For each logical item:
/// - if the enclosing batch `end_code != 0`, the item's `end_code` is set and
///   `payload` left as `None`;
/// - otherwise, the function slices the underlying batch payload according to
///   the batch layout and stores the slice in `payload`.
pub fn merge_read_raw(
    batches: &[ReadBatchSpec],
    batch_results: &[ReadBatchResult],
) -> Vec<McReadItemRaw> {
    // Determine the maximum index across all batches to size the result vector.
    let mut max_index = 0usize;
    for b in batches.iter() {
        for e in b.entries.iter() {
            if e.index > max_index {
                max_index = e.index;
            }
        }
    }

    let total = max_index + 1;
    let mut results: Vec<McReadItemRaw> = (0..total)
        .map(|idx| McReadItemRaw {
            index: idx,
            end_code: 0,
            payload: None,
        })
        .collect();

    for (batch, res) in batches.iter().zip(batch_results.iter()) {
        let unit_size = batch.device.unit_size();

        if res.end_code != 0 {
            // Whole batch failed with a non-zero completion code.
            for entry in batch.entries.iter() {
                if entry.index < results.len() {
                    results[entry.index].end_code = res.end_code;
                    results[entry.index].payload = None;
                }
            }
            continue;
        }

        let payload = match &res.payload {
            Some(p) => p,
            None => {
                // No payload provided; leave items as default (no value).
                continue;
            }
        };

        let expected_bytes = (batch.points as usize) * unit_size;
        if payload.len() < expected_bytes {
            // Payload too short for declared points; skip this batch.
            continue;
        }

        for entry in batch.entries.iter() {
            let offset_bytes = (entry.offset_words as usize) * unit_size;
            let len_bytes = (entry.len_words as usize) * unit_size;
            if offset_bytes
                .checked_add(len_bytes)
                .map(|end| end <= payload.len())
                != Some(true)
            {
                continue;
            }

            if entry.index >= results.len() {
                continue;
            }

            let slice = payload.slice(offset_bytes..offset_bytes + len_bytes);
            results[entry.index].end_code = 0;
            results[entry.index].payload = Some(slice);
        }
    }

    results
}

#[inline]
fn bi_loop_execute<F>(
    word_total: usize,
    dword_total: usize,
    mut predicate: F,
    mut consumer: impl FnMut(usize, usize, usize, usize),
) where
    F: FnMut(usize, usize) -> bool,
{
    let mut w_off = 0usize;
    let mut w_len = 0usize;
    let mut d_off = 0usize;
    let mut d_len = 0usize;

    let in_range = |off: usize, len: usize, total: usize| off + len < total;

    while in_range(w_off, w_len, word_total) || in_range(d_off, d_len, dword_total) {
        if w_off < word_total && in_range(w_off, w_len, word_total) {
            // Only within first group
            if predicate(w_len, d_len) {
                consumer(w_off, w_len, d_off, d_len);
                w_off += w_len;
                w_len = 0;
            } else {
                w_len += 1;
            }
        } else if w_off < word_total
            && !in_range(w_off, w_len, word_total)
            && in_range(d_off, d_len, dword_total)
        {
            // Crossing from first to second group
            if predicate(w_len, d_len) {
                consumer(w_off, w_len, d_off, d_len);
                w_off = word_total;
                w_len = 0;
                d_off += d_len;
                d_len = 0;
            } else {
                d_len += 1;
            }
        } else if d_off < dword_total && in_range(d_off, d_len, dword_total) {
            // Only within second group
            if predicate(w_len, d_len) {
                consumer(w_off, w_len, d_off, d_len);
                d_off += d_len;
                d_len = 0;
            } else {
                d_len += 1;
            }
        } else {
            d_len += 1;
        }
    }

    if w_off < word_total || d_off < dword_total {
        consumer(
            w_off,
            word_total.saturating_sub(w_off),
            d_off,
            dword_total.saturating_sub(d_off),
        );
    }
}

/// Plan MC random read batches for word/dword items under series-specific limits.
///
/// This mirrors the Java `McNetwork.readDeviceRandomInWord` behaviour using a
/// bi-loop strategy over word and dword address lists.
pub fn plan_random_read_batches(
    series: McSeries,
    word_entries: Vec<RandomReadEntry>,
    dword_entries: Vec<RandomReadEntry>,
) -> Vec<RandomReadBatchSpec> {
    let mut batches = Vec::new();
    if word_entries.is_empty() && dword_entries.is_empty() {
        return batches;
    }

    let max_len = series.device_random_read_in_word_points_max() as usize;
    let w_total = word_entries.len();
    let d_total = dword_entries.len();

    bi_loop_execute(
        w_total,
        d_total,
        |w_len, d_len| {
            if max_len == 0 {
                // Safety: never create infinite loop; treat 0 as "no limit" here.
                false
            } else {
                w_len + d_len >= max_len
            }
        },
        |w_off, w_len, d_off, d_len| {
            if w_len == 0 && d_len == 0 {
                return;
            }
            let mut word_batch = Vec::with_capacity(w_len);
            let mut dword_batch = Vec::with_capacity(d_len);
            if w_off < w_total && w_len > 0 {
                word_batch.extend(word_entries[w_off..w_off + w_len].iter().cloned());
            }
            if d_off < d_total && d_len > 0 {
                dword_batch.extend(dword_entries[d_off..d_off + d_len].iter().cloned());
            }
            batches.push(RandomReadBatchSpec {
                word_entries: word_batch,
                dword_entries: dword_batch,
            });
        },
    );

    batches
}

/// Plan MC random write batches for word/dword items under series-specific limits.
///
/// This mirrors the Java `McNetwork.writeDeviceRandomInWord` behaviour where
/// the effective batch size is constrained by a weighted combination of word
/// and dword entries.
pub fn plan_random_write_batches(
    series: McSeries,
    word_entries: Vec<RandomWriteEntry>,
    dword_entries: Vec<RandomWriteEntry>,
) -> Vec<RandomWriteBatchSpec> {
    let mut batches = Vec::new();
    if word_entries.is_empty() && dword_entries.is_empty() {
        return batches;
    }

    let max_len = series.device_random_write_in_word_points_max() as usize;
    let w_total = word_entries.len();
    let d_total = dword_entries.len();

    bi_loop_execute(
        w_total,
        d_total,
        |w_len, d_len| {
            if max_len == 0 {
                false
            } else {
                // Java uses `i1.len * 12 + i2.len * 14 >= maxLength` as the
                // batch splitting condition for random write.
                w_len.saturating_mul(12) + d_len.saturating_mul(14) >= max_len
            }
        },
        |w_off, w_len, d_off, d_len| {
            if w_len == 0 && d_len == 0 {
                return;
            }
            let mut word_batch = Vec::with_capacity(w_len);
            let mut dword_batch = Vec::with_capacity(d_len);
            if w_off < w_total && w_len > 0 {
                word_batch.extend(word_entries[w_off..w_off + w_len].iter().cloned());
            }
            if d_off < d_total && d_len > 0 {
                dword_batch.extend(dword_entries[d_off..d_off + d_len].iter().cloned());
            }
            batches.push(RandomWriteBatchSpec {
                word_entries: word_batch,
                dword_entries: dword_batch,
            });
        },
    );

    batches
}
