use super::{
    super::error::{Error, ErrorCode, Result},
    types::S7PduType,
};
use bytes::BufMut;

/// S7 Header
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S7Header {
    pub protocol_id: u8,
    pub pdu_type: S7PduType,
    pub reserved: u16,
    pub pdu_ref: u16,
    pub param_len: u16,
    pub payload_len: u16,
    pub error_code: Option<ErrorCode>,
}

impl S7Header {
    /// Parse an S7 header from bytes. Expects at least 10 bytes.
    /// For Ack/AckData PDUs, this will also parse and skip the 4-byte Ack extension
    /// and populate `ack` accordingly.
    pub fn parse(input: &[u8]) -> Result<(S7Header, &[u8])> {
        if input.len() < 10 {
            return Err(Error::InsufficientData {
                needed: 10,
                available: input.len(),
            });
        }
        let protocol_id = input[0];
        if protocol_id != 0x32 {
            return Err(Error::ErrInvalidFrame);
        }
        let pdu_type = S7PduType::try_from(input[1]).map_err(|_| Error::ErrInvalidFrame)?;
        let reserved = u16::from_be_bytes([input[2], input[3]]);
        let pdu_ref = u16::from_be_bytes([input[4], input[5]]);
        let param_len = u16::from_be_bytes([input[6], input[7]]);
        let payload_len = u16::from_be_bytes([input[8], input[9]]);

        let mut rest = &input[10..];
        let error_code = match pdu_type {
            S7PduType::Ack | S7PduType::AckData => {
                if rest.len() < 2 {
                    return Err(Error::InsufficientData {
                        needed: 2,
                        available: rest.len(),
                    });
                }
                let error_code_raw = u16::from_be_bytes([rest[0], rest[1]]);
                let error_code =
                    ErrorCode::try_from(error_code_raw).map_err(|_| Error::ErrInvalidFrame)?;
                rest = &rest[2..];
                Some(error_code)
            }
            _ => None,
        };
        Ok((
            S7Header {
                protocol_id,
                pdu_type,
                reserved,
                pdu_ref,
                param_len,
                payload_len,
                error_code,
            },
            rest,
        ))
    }

    /// Encode S7 header to buffer. If `pdu_type` is Ack/AckData and `ack` is present,
    pub fn encode_to<B: BufMut>(&self, dst: &mut B) {
        dst.put_u8(self.protocol_id);
        dst.put_u8(self.pdu_type as u8);
        dst.put_u16(self.reserved);
        dst.put_u16(self.pdu_ref);
        dst.put_u16(self.param_len);
        dst.put_u16(self.payload_len);
        if matches!(self.pdu_type, S7PduType::Ack | S7PduType::AckData) {
            if let Some(error_code) = self.error_code {
                dst.put_u16(error_code as u16);
            }
        }
    }
}
