use super::{
    frame::{
        header::{
            McHeader, McHeader1EAck, McHeader1EReq, McHeader3EAck, McHeader3EReq, McHeader4EAck,
            McHeader4EReq, McHeaderAckCommon, McHeaderReqCommon,
        },
        route::{McRoute1E, McRoute3E4E},
        McAppBody, McMessage,
    },
    types::McFrameVariant,
};
use bytes::{BufMut, Bytes, BytesMut};
use std::io;
use thiserror::Error;
use tokio_util::codec::{Decoder, Encoder};

/// MC codec error type for header (de)serialization.
#[derive(Debug, Error)]
pub enum McCodecError {
    /// Input buffer is too short for the expected header.
    #[error("buffer too short for MC header: expected at least {expected} bytes, got {actual}")]
    BufferTooShort { expected: usize, actual: usize },
    /// Unsupported frame variant for the requested operation.
    #[error("unsupported MC frame variant: {0:?}")]
    UnsupportedVariant(McFrameVariant),
}

/// Unified MC codec for binary frames (1E/3E/4E).
///
/// This codec is aligned with the S7 codec design: it provides a single
/// `Decoder/Encoder` implementation that encapsulates the on-wire framing
/// for the protocol.
#[derive(Debug, Clone)]
pub struct Codec {
    /// Frame variant to decode (1E/3E/4E, binary/ASCII).
    pub frame_variant: McFrameVariant,
}

impl Default for Codec {
    fn default() -> Self {
        Self {
            frame_variant: McFrameVariant::Frame3EBinary,
        }
    }
}

impl Decoder for Codec {
    type Item = McMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.frame_variant {
            McFrameVariant::Frame3EBinary => decode_3e_ack(src),
            McFrameVariant::Frame4EBinary => decode_4e_ack(src),
            McFrameVariant::Frame1EBinary => {
                // 1E streaming decode is intentionally not implemented. The
                // current gateway version focuses on 3E/4E binary frames; 1E
                // support can be added later using dedicated request/response
                // builders without affecting this codec.
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "MC 1E frame decoding is not supported by McCodec",
                ))
            }
            McFrameVariant::Frame3EAscii | McFrameVariant::Frame4EAscii => {
                // ASCII 3E/4E frames are not supported in this version of the
                // driver. Configuration should prevent selecting ASCII frame
                // variants so that they never reach the codec at runtime.
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "MC ASCII 3E/4E frame decoding is not supported",
                ))
            }
        }
    }
}

impl Encoder<McMessage> for Codec {
    type Error = io::Error;

    fn encode(&mut self, item: McMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match (item.header, item.body) {
            (McHeader::Ack1E(header), Some(McAppBody::Raw(payload))) => {
                let mut h = encode_header_1e_req(&McHeader1EReq {
                    // For ACK, timer is not used; mirror fields into a req-style helper.
                    common: McHeaderReqCommon {
                        monitoring_timer: 0,
                    },
                    sub_header: header.sub_header,
                    route: McRoute1E { pc_number: 0 },
                });
                dst.reserve(h.len() + payload.len());
                dst.extend_from_slice(&h.split());
                dst.extend_from_slice(&payload);
            }
            (McHeader::Ack3E(header), Some(McAppBody::Raw(payload))) => {
                // For ACK, data_length is header.data_length; we simply stream
                // header bytes followed by payload for diagnostics/testing.
                let mut buf = BytesMut::with_capacity(McHeader3EReq::byte_len());
                buf.put_u16_le(header.sub_header);
                put_route_3e4e(&mut buf, header.route);
                buf.put_u16_le(header.data_length);
                // monitoring timer is not part of Ack header on wire; keep 0.
                buf.put_u16_le(0);
                dst.reserve(buf.len() + payload.len());
                dst.extend_from_slice(&buf.split());
                dst.extend_from_slice(&payload);
            }
            (McHeader::Ack4E(header), Some(McAppBody::Raw(payload))) => {
                let mut buf = BytesMut::with_capacity(McHeader4EReq::byte_len());
                buf.put_u16_le(header.sub_header);
                buf.put_u16_le(header.serial_number);
                buf.put_u16_le(header.fixed_number);
                put_route_3e4e(&mut buf, header.route);
                buf.put_u16_le(header.data_length);
                buf.put_u16_le(0);
                dst.reserve(buf.len() + payload.len());
                dst.extend_from_slice(&buf.split());
                dst.extend_from_slice(&payload);
            }
            // Request encoding: McHeader::Req3E/Req4E with Parsed(McPdu).
            (McHeader::Req3E(header), Some(McAppBody::Parsed(pdu))) => {
                let body = pdu.encode_to_bytes();
                let mut h = header.clone();
                // MC 3E request header `data_length` counts from the monitoring
                // timer field to the last byte of the request body, i.e.
                // `2 (monitoring timer) + body.len()`.
                let data_len = body.len().saturating_add(2).min(u16::MAX as usize) as u16;
                h.data_length = data_len;
                let mut buf = encode_header_3e_req(&h);
                dst.reserve(buf.len() + body.len());
                dst.extend_from_slice(&buf.split());
                dst.extend_from_slice(&body);
            }
            (McHeader::Req4E(header), Some(McAppBody::Parsed(pdu))) => {
                let body = pdu.encode_to_bytes();
                let mut h = header.clone();
                // Same semantics as 3E: `data_length` covers monitoring timer
                // plus the request body bytes.
                let data_len = body.len().saturating_add(2).min(u16::MAX as usize) as u16;
                h.data_length = data_len;
                let mut buf = encode_header_4e_req(&h);
                dst.reserve(buf.len() + body.len());
                dst.extend_from_slice(&buf.split());
                dst.extend_from_slice(&body);
            }
            _ => {}
        }
        Ok(())
    }
}

/// Encode a 1E binary request header into a new `BytesMut` buffer.
pub fn encode_header_1e_req(header: &McHeader1EReq) -> BytesMut {
    let mut buf = BytesMut::with_capacity(McHeader1EReq::byte_len());
    buf.put_u8(header.sub_header);
    buf.put_u8(header.route.pc_number);
    buf.put_u16_le(header.common.monitoring_timer);
    buf
}

/// Encode a 3E binary request header into a new `BytesMut` buffer.
pub fn encode_header_3e_req(header: &McHeader3EReq) -> BytesMut {
    let mut buf = BytesMut::with_capacity(McHeader3EReq::byte_len());
    buf.put_u16_le(header.sub_header);
    put_route_3e4e(&mut buf, header.route);
    buf.put_u16_le(header.data_length);
    buf.put_u16_le(header.common.monitoring_timer);
    buf
}

/// Encode a 4E binary request header into a new `BytesMut` buffer.
pub fn encode_header_4e_req(header: &McHeader4EReq) -> BytesMut {
    let mut buf = BytesMut::with_capacity(McHeader4EReq::byte_len());
    buf.put_u16_le(header.sub_header);
    buf.put_u16_le(header.serial_number);
    buf.put_u16_le(header.fixed_number);
    put_route_3e4e(&mut buf, header.route);
    buf.put_u16_le(header.data_length);
    buf.put_u16_le(header.common.monitoring_timer);
    buf
}

#[allow(unused)]
/// Decode a 1E binary acknowledge header from a byte slice.
pub fn decode_header_1e_ack(
    buf: &[u8],
    variant: McFrameVariant,
) -> Result<McHeader1EAck, McCodecError> {
    if !matches!(variant, McFrameVariant::Frame1EBinary) {
        return Err(McCodecError::UnsupportedVariant(variant));
    }
    let expected = 2usize;
    if buf.len() < expected {
        return Err(McCodecError::BufferTooShort {
            expected,
            actual: buf.len(),
        });
    }

    let sub_header = buf[0];
    let end_code = buf[1];
    Ok(McHeader1EAck {
        common: McHeaderAckCommon {
            frame_variant: variant,
        },
        sub_header,
        end_code,
    })
}

/// Decode a 3E binary acknowledge header from a byte slice.
pub fn decode_header_3e_ack(
    buf: &[u8],
    variant: McFrameVariant,
) -> Result<McHeader3EAck, McCodecError> {
    if !matches!(variant, McFrameVariant::Frame3EBinary) {
        return Err(McCodecError::UnsupportedVariant(variant));
    }
    let expected = 2 + McRoute3E4E::byte_len() + 2 + 2;
    if buf.len() < expected {
        return Err(McCodecError::BufferTooShort {
            expected,
            actual: buf.len(),
        });
    }
    let mut idx = 0usize;
    let sub_header = u16::from_le_bytes([buf[idx], buf[idx + 1]]);
    idx += 2;
    let route = parse_route_3e4e(&buf[idx..idx + McRoute3E4E::byte_len()]);
    idx += McRoute3E4E::byte_len();
    let data_length = u16::from_le_bytes([buf[idx], buf[idx + 1]]);
    idx += 2;
    let end_code = u16::from_le_bytes([buf[idx], buf[idx + 1]]);

    Ok(McHeader3EAck {
        common: McHeaderAckCommon {
            frame_variant: variant,
        },
        sub_header,
        route,
        data_length,
        end_code,
    })
}

/// Decode a 4E binary acknowledge header from a byte slice.
pub fn decode_header_4e_ack(
    buf: &[u8],
    variant: McFrameVariant,
) -> Result<McHeader4EAck, McCodecError> {
    if !matches!(variant, McFrameVariant::Frame4EBinary) {
        return Err(McCodecError::UnsupportedVariant(variant));
    }
    let expected = 2 + 2 + 2 + McRoute3E4E::byte_len() + 2 + 2;
    if buf.len() < expected {
        return Err(McCodecError::BufferTooShort {
            expected,
            actual: buf.len(),
        });
    }
    let mut idx = 0usize;
    let sub_header = u16::from_le_bytes([buf[idx], buf[idx + 1]]);
    idx += 2;
    let serial_number = u16::from_le_bytes([buf[idx], buf[idx + 1]]);
    idx += 2;
    let fixed_number = u16::from_le_bytes([buf[idx], buf[idx + 1]]);
    idx += 2;
    let route = parse_route_3e4e(&buf[idx..idx + McRoute3E4E::byte_len()]);
    idx += McRoute3E4E::byte_len();
    let data_length = u16::from_le_bytes([buf[idx], buf[idx + 1]]);
    idx += 2;
    let end_code = u16::from_le_bytes([buf[idx], buf[idx + 1]]);

    Ok(McHeader4EAck {
        common: McHeaderAckCommon {
            frame_variant: variant,
        },
        sub_header,
        serial_number,
        fixed_number,
        route,
        data_length,
        end_code,
    })
}

/// Helper: encode a 3E/4E route into the buffer (little-endian for u16 field).
fn put_route_3e4e(buf: &mut BytesMut, route: McRoute3E4E) {
    buf.put_u8(route.network_number);
    buf.put_u8(route.pc_number);
    buf.put_u16_le(route.module_io_number);
    buf.put_u8(route.station_number);
}

/// Helper: parse a 3E/4E route from a 5-byte slice.
fn parse_route_3e4e(buf: &[u8]) -> McRoute3E4E {
    let mut idx = 0usize;
    let network_number = buf[idx];
    idx += 1;
    let pc_number = buf[idx];
    idx += 1;
    let module_io_number = u16::from_le_bytes([buf[idx], buf[idx + 1]]);
    idx += 2;
    let station_number = buf[idx];
    McRoute3E4E {
        network_number,
        pc_number,
        module_io_number,
        station_number,
    }
}

/// Internal helper: streaming decode for 3E binary acknowledge frames.
fn decode_3e_ack(src: &mut BytesMut) -> Result<Option<McMessage>, io::Error> {
    // Minimum header bytes for 3E ack: 2(sub) + 5(route) + 2(len) + 2(end) = 11
    const HEADER_LEN: usize = 2 + McRoute3E4E::byte_len() + 2 + 2;
    const FIXED_WITHOUT_END: usize = 2 + McRoute3E4E::byte_len() + 2;

    if src.len() < HEADER_LEN {
        return Ok(None);
    }

    // Peek header to obtain data_length
    let buf = &src[..HEADER_LEN];
    let header = decode_header_3e_ack(buf, McFrameVariant::Frame3EBinary)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let frame_len = FIXED_WITHOUT_END + header.data_length as usize;
    if src.len() < frame_len {
        return Ok(None);
    }

    let frame_bytes = src.split_to(frame_len).freeze();
    let payload_offset = HEADER_LEN;
    let payload = if frame_len > payload_offset {
        frame_bytes.slice(payload_offset..frame_len)
    } else {
        Bytes::new()
    };

    Ok(Some(McMessage {
        header: McHeader::Ack3E(header),
        body: Some(McAppBody::Raw(payload)),
    }))
}

/// Internal helper: streaming decode for 4E binary acknowledge frames.
fn decode_4e_ack(src: &mut BytesMut) -> Result<Option<McMessage>, io::Error> {
    // Minimum header bytes for 4E ack:
    // 2(sub) + 2(serial) + 2(fixed) + 5(route) + 2(len) + 2(end) = 15
    const HEADER_LEN: usize = 2 + 2 + 2 + McRoute3E4E::byte_len() + 2 + 2;
    const FIXED_WITHOUT_END: usize = 2 + 2 + 2 + McRoute3E4E::byte_len() + 2;

    if src.len() < HEADER_LEN {
        return Ok(None);
    }

    let buf = &src[..HEADER_LEN];
    let header = decode_header_4e_ack(buf, McFrameVariant::Frame4EBinary)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let frame_len = FIXED_WITHOUT_END + header.data_length as usize;
    if src.len() < frame_len {
        return Ok(None);
    }

    let frame_bytes = src.split_to(frame_len).freeze();
    let payload_offset = HEADER_LEN;
    let payload = if frame_len > payload_offset {
        frame_bytes.slice(payload_offset..frame_len)
    } else {
        Bytes::new()
    };

    Ok(Some(McMessage {
        header: McHeader::Ack4E(header),
        body: Some(McAppBody::Raw(payload)),
    }))
}
