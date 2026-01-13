use super::{
    super::types::McSeries,
    command::{McCommandCode, McCommandKind},
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// MC protocol data unit body for requests and responses.
///
/// This module defines only the wire-level shapes for common MC commands used
/// by the gateway. Higher layers (driver/codec) are responsible for mapping
/// these payloads to typed values and enforcing semantic validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum McRequestBody {
    /// Device access: batch read units (3E/4E).
    ///
    /// This request reads a continuous block of elements starting from the
    /// given head device number. The interpretation of `points` (bits/words/
    /// dwords) depends on the target device type and command code.
    DeviceAccessBatchReadUnits {
        /// Head device number.
        head: u32,
        /// Number of points to read.
        points: u16,
        /// Raw device code byte as defined by MC protocol.
        device_code: u16,
    },
    /// Device access: batch write units (3E/4E).
    ///
    /// This request writes a continuous block of elements starting from the
    /// head device number. The payload is encoded according to MC binary
    /// representation rules for the selected device type.
    DeviceAccessBatchWriteUnits {
        /// Head device number.
        head: u32,
        /// Number of points to write.
        points: u16,
        /// Raw device code byte as defined by MC protocol.
        device_code: u16,
        /// Encoded payload bytes.
        data: Bytes,
    },
    /// Device access: random read units (3E/4E).
    ///
    /// This request reads multiple non-contiguous word/dword addresses in a
    /// single MC command. The `words`/`dwords` lists are encoded following the
    /// MC specification: each entry carries head-device number and device code
    /// without an explicit points count (count is always 1 for random reads).
    DeviceAccessRandomReadUnits {
        /// Word-sized device addresses (head device number + device code).
        ///
        /// Each entry represents a single word unit at the given head address.
        word_addrs: Vec<(u32, u16)>,
        /// Dword-sized device addresses (head device number + device code).
        ///
        /// Each entry represents a single dword unit (2 words) at the given
        /// head address.
        dword_addrs: Vec<(u32, u16)>,
    },
    /// Device access: random write units (3E/4E).
    ///
    /// This request writes multiple non-contiguous word/dword addresses in a
    /// single MC command. Payloads are provided as raw MC-encoded bytes per
    /// address; higher layers are responsible for packing scalar values into
    /// the appropriate representation.
    DeviceAccessRandomWriteUnits {
        /// Word-sized device addresses and payloads.
        ///
        /// Each entry is `(head, device_code, data)` where `data` is the MC
        /// encoded word payload for the address.
        word_items: Vec<(u32, u16, Bytes)>,
        /// Dword-sized device addresses and payloads.
        ///
        /// Each entry is `(head, device_code, data)` where `data` is the MC
        /// encoded dword payload for the address.
        dword_items: Vec<(u32, u16, Bytes)>,
    },
    /// Raw request payload for commands that are not yet modeled.
    ///
    /// This variant allows the planner/driver to build fully encoded MC
    /// command bodies while the semantic structure for a given command is
    /// still under development.
    Raw {
        /// Fully encoded MC request payload bytes.
        ///
        /// NOTE: we intentionally use `Vec<u8>` here rather than `Bytes`
        /// because this type is serialized/deserialized via `serde` as part
        /// of runtime metadata, and `Bytes` does not implement `Serialize`/
        /// `Deserialize` without additional feature flags.
        payload: Vec<u8>,
    },
    /// Placeholder for other commands not yet modeled.
    ///
    /// This variant keeps the PDU decoder forward compatible with
    /// unimplemented command kinds.
    #[serde(other)]
    Unsupported,
}

/// MC response body grouped by command kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum McResponseBody {
    /// Device access: batch read units ack (3E/4E).
    ///
    DeviceAccessBatchReadUnits {
        /// Raw payload bytes returned by the PLC for the requested block.
        ///
        /// The layout of this payload depends on the device type and the
        /// number of points requested. Higher layers interpret this buffer
        /// according to the associated `McLogicalAddress` and `DataType`.
        payload: Vec<u8>,
    },
    /// Device access: batch write units ack (3E/4E).
    ///
    DeviceAccessBatchWriteUnits {
        /// End code mirrored from the MC header. A value of `0x0000` indicates
        /// success; any non-zero value represents a protocol-level error.
        end_code: u16,
    },
    /// Raw response payload for commands that are not yet modeled.
    ///
    /// This variant is primarily used by the codec/session layer to return
    /// complete MC responses to higher layers without forcing eager semantic
    /// decoding.
    Raw {
        /// Fully encoded MC response payload bytes.
        ///
        /// NOTE: we intentionally use `Vec<u8>` here rather than `Bytes`
        /// because this type is serialized/deserialized via `serde` as part
        /// of runtime metadata, and `Bytes` does not implement `Serialize`/
        /// `Deserialize` without additional feature flags.
        payload: Vec<u8>,
    },
    /// Placeholder for other commands not yet modeled.
    #[serde(other)]
    Unsupported,
}

/// Unified MC PDU containing either request or response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McBody {
    Request(McRequestBody),
    Response(McResponseBody),
}

/// High-level MC protocol data unit (PDU) used by codec and session layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McPdu {
    /// PLC series for this PDU, used to derive subcommand and minor layout
    /// differences (e.g. IQ-R vs QnA).
    pub series: McSeries,
    /// Semantic command kind.
    pub command: McCommandKind,
    /// Typed request/response body.
    pub body: McBody,
}

impl McPdu {
    /// Create a new PDU instance for the given series, command kind and body.
    #[inline]
    pub fn new(series: McSeries, command: McCommandKind, body: McBody) -> Self {
        Self {
            series,
            command,
            body,
        }
    }

    /// Encode this PDU into a binary payload buffer (command body only).
    ///
    /// For 3E/4E frames this function encodes the command in the same layout as
    /// the Java `McReqData` family:
    ///
    /// - 2 bytes: command code (little-endian)
    /// - 2 bytes: subcommand (little-endian)
    /// - N bytes: command-specific payload
    pub fn encode_to_bytes(&self) -> Bytes {
        if let McBody::Request(body) = &self.body {
            match body {
                McRequestBody::DeviceAccessBatchReadUnits {
                    head,
                    points,
                    device_code,
                } => {
                    let mut v = Vec::with_capacity(2 + 2 + 4 + 2 + 2);
                    let cmd = self.command.to_code();
                    if let McCommandCode::Code3E4E(code) = cmd {
                        v.extend_from_slice(&code.to_le_bytes());
                    }
                    let sub = self.command.default_subcommand(self.series);

                    v.extend_from_slice(&sub.to_le_bytes());
                    v.extend_from_slice(&(*head).to_le_bytes());
                    v.extend_from_slice(&points.to_le_bytes());
                    v.extend_from_slice(&device_code.to_le_bytes());
                    return Bytes::from(v);
                }
                McRequestBody::DeviceAccessBatchWriteUnits {
                    head,
                    points,
                    device_code,
                    data,
                } => {
                    let mut v = Vec::with_capacity(2 + 2 + 4 + 2 + 2 + data.len());
                    let cmd = self.command.to_code();
                    if let McCommandCode::Code3E4E(code) = cmd {
                        v.extend_from_slice(&code.to_le_bytes());
                    }
                    let sub = self.command.default_subcommand(self.series);

                    v.extend_from_slice(&sub.to_le_bytes());
                    v.extend_from_slice(&(*head).to_le_bytes());
                    v.extend_from_slice(&points.to_le_bytes());
                    v.extend_from_slice(&device_code.to_le_bytes());
                    v.extend_from_slice(data.as_ref());
                    return Bytes::from(v);
                }
                McRequestBody::DeviceAccessRandomReadUnits {
                    word_addrs,
                    dword_addrs,
                } => {
                    // Layout (3E/4E, binary), mirrors Java
                    // `McReadDeviceRandomInWordReqData.toByteArray()`:
                    //
                    // - 2 bytes: command
                    // - 2 bytes: subcommand
                    // - 1 byte : word address count
                    // - 1 byte : dword address count
                    // - N bytes: word addresses (device code + head)
                    // - M bytes: dword addresses (device code + head)
                    let cmd = self.command.to_code();
                    let sub = self.command.default_subcommand(self.series);

                    let addr_len_word = device_addr_len(self.series);
                    let addr_len_dword = device_addr_len(self.series);

                    let word_count =
                        u8::try_from(word_addrs.len().min(u8::MAX as usize)).unwrap_or(u8::MAX);
                    let dword_count =
                        u8::try_from(dword_addrs.len().min(u8::MAX as usize)).unwrap_or(u8::MAX);

                    let mut v = Vec::with_capacity(
                        2 + 2
                            + 1
                            + 1
                            + (word_count as usize) * addr_len_word
                            + (dword_count as usize) * addr_len_dword,
                    );

                    if let McCommandCode::Code3E4E(code) = cmd {
                        v.extend_from_slice(&code.to_le_bytes());
                    }
                    v.extend_from_slice(&sub.to_le_bytes());
                    v.push(word_count);
                    v.push(dword_count);

                    for (head, device_code) in word_addrs.iter().take(word_count as usize) {
                        encode_device_addr_without_points(self.series, *head, *device_code, &mut v);
                    }
                    for (head, device_code) in dword_addrs.iter().take(dword_count as usize) {
                        encode_device_addr_without_points(self.series, *head, *device_code, &mut v);
                    }

                    return Bytes::from(v);
                }
                McRequestBody::DeviceAccessRandomWriteUnits {
                    word_items,
                    dword_items,
                } => {
                    // Layout (3E/4E, binary), mirrors Java
                    // `McWriteDeviceRandomInWordReqData.toByteArray()` for 3E:
                    //
                    // - 2 bytes: command
                    // - 2 bytes: subcommand
                    // - 1 byte : word content count
                    // - 1 byte : dword content count
                    // - N bytes: word contents (device addr without points + data)
                    // - M bytes: dword contents (device addr without points + data)
                    let cmd = self.command.to_code();
                    let sub = self.command.default_subcommand(self.series);

                    let addr_len_word = device_addr_len(self.series);
                    let addr_len_dword = device_addr_len(self.series);

                    let word_count =
                        u8::try_from(word_items.len().min(u8::MAX as usize)).unwrap_or(u8::MAX);
                    let dword_count =
                        u8::try_from(dword_items.len().min(u8::MAX as usize)).unwrap_or(u8::MAX);

                    let data_bytes_word: usize = word_items
                        .iter()
                        .take(word_count as usize)
                        .map(|(_, _, data)| data.len())
                        .sum();
                    let data_bytes_dword: usize = dword_items
                        .iter()
                        .take(dword_count as usize)
                        .map(|(_, _, data)| data.len())
                        .sum();

                    let mut v = Vec::with_capacity(
                        2 + 2
                            + 1
                            + 1
                            + (word_count as usize) * addr_len_word
                            + (dword_count as usize) * addr_len_dword
                            + data_bytes_word
                            + data_bytes_dword,
                    );

                    if let McCommandCode::Code3E4E(code) = cmd {
                        v.extend_from_slice(&code.to_le_bytes());
                    }
                    v.extend_from_slice(&sub.to_le_bytes());
                    v.push(word_count);
                    v.push(dword_count);

                    for (head, device_code, data) in word_items.iter().take(word_count as usize) {
                        encode_device_addr_without_points(self.series, *head, *device_code, &mut v);
                        v.extend_from_slice(data);
                    }
                    for (head, device_code, data) in dword_items.iter().take(dword_count as usize) {
                        encode_device_addr_without_points(self.series, *head, *device_code, &mut v);
                        v.extend_from_slice(data);
                    }

                    return Bytes::from(v);
                }
                McRequestBody::Raw { payload } => {
                    return Bytes::from(payload.clone());
                }
                _ => {
                    // For unimplemented request bodies we currently return an
                    // empty payload. Concrete command implementations will be
                    // added as the corresponding features are introduced.
                    return Bytes::new();
                }
            }
        }
        Bytes::new()
    }

    /// Decode an MC PDU from raw payload bytes and a known command kind.
    #[allow(unused)]
    pub fn decode_from_bytes(kind: McCommandKind, payload: Bytes) -> Self {
        let body = match kind {
            McCommandKind::DeviceAccessBatchReadUnits => {
                McResponseBody::DeviceAccessBatchReadUnits {
                    payload: payload.to_vec(),
                }
            }
            McCommandKind::DeviceAccessBatchWriteUnits => {
                // For batch write units, the MC header already carries the end
                // code; the payload is typically empty. We mirror a zero end
                // code here and let callers rely on the header for details.
                McResponseBody::DeviceAccessBatchWriteUnits { end_code: 0 }
            }
            _ => McResponseBody::Raw {
                payload: payload.to_vec(),
            },
        };
        // For response decoding we currently default the series to QnA. Callers
        // that require precise series-specific behaviour should construct the
        // PDU via `McPdu::new` with an explicit series.
        McPdu::new(McSeries::QnA, kind, McBody::Response(body))
    }
}

/// Return the on-wire byte length of a device address (without points count)
/// for the given PLC series.
///
/// This mirrors the Java `McDeviceAddress.byteArrayLengthWithoutPointsCount`
/// semantics for 3E/4E frames:
///
/// - QnA/Q/L : 3 bytes head device number + 1 byte device code
/// - iQ-R/A  : 4 bytes head device number + 2 bytes device code
fn device_addr_len(series: McSeries) -> usize {
    match series {
        McSeries::QnA | McSeries::QL => 3 + 1,
        McSeries::IQR | McSeries::A => 4 + 2,
    }
}

/// Encode a device address (without points count) into the provided buffer.
///
/// The layout is aligned with Java `McDeviceAddress.toByteArrayWithoutPointsCount`:
///
/// - QnA/Q/L : head device number as 3-byte little-endian + 1-byte device code
/// - iQ-R/A  : head device number as 4-byte little-endian + 2-byte device code
fn encode_device_addr_without_points(
    series: McSeries,
    head: u32,
    device_code: u16,
    out: &mut Vec<u8>,
) {
    match series {
        McSeries::QnA | McSeries::QL => {
            let bytes = head.to_le_bytes();
            out.push(bytes[0]);
            out.push(bytes[1]);
            out.push(bytes[2]);
            out.push(device_code as u8);
        }
        McSeries::IQR | McSeries::A => {
            out.extend_from_slice(&head.to_le_bytes());
            out.extend_from_slice(&device_code.to_le_bytes());
        }
    }
}
