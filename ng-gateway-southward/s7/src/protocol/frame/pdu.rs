use super::owned::{S7ParamOwned, S7PayloadOwned};
use super::{
    super::error::{Error, ErrorCode, Result},
    comm::S7Header,
    parse_param_ref, parse_payload_ref,
    r#ref::{S7ParamRef, S7PayloadRef},
    types::S7PduType,
    WireDecode, WireEncode,
};
use bytes::{BufMut, Bytes, BytesMut};

/// Unified S7 PDU container (header + parameter + payload) with zero-copy slices
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S7Pdu {
    pub header: S7Header,
    pub param: Bytes,
    pub payload: Bytes,
}

impl S7Pdu {
    /// Validate against error_code (if any) for Ack/AckData.
    pub fn validate_response(&self) -> Result<()> {
        if matches!(self.header.pdu_type, S7PduType::Ack | S7PduType::AckData) {
            if let Some(code) = self.header.error_code {
                if code != ErrorCode::Success {
                    return Err(Error::S7Error { code });
                }
            }
        }
        Ok(())
    }

    /// Materialize into owned Bytes
    pub fn into_bytes(self) -> Bytes {
        let mut buf = BytesMut::with_capacity(self.encoded_len(&()));
        self.header.encode_to(&mut buf);
        if !self.param.is_empty() {
            buf.put_slice(&self.param);
        }
        if !self.payload.is_empty() {
            buf.put_slice(&self.payload);
        }
        buf.freeze()
    }

    /// Construct from structured owned parameter and payload.
    ///
    /// This encodes `param_owned` and `payload_owned` once into `Bytes` and returns
    /// a zero-copy `S7Pdu` suitable for outbound transmission. The caller is
    /// responsible for providing a header with correct `param_len` and `payload_len`.
    pub fn from_owned(
        mut header: S7Header,
        param_owned: S7ParamOwned,
        payload_owned: S7PayloadOwned,
    ) -> S7Pdu {
        let mut p = BytesMut::with_capacity(param_owned.encoded_len(&()));
        let _ = param_owned.encode_to(&mut p, &());
        let param = p.freeze();

        let mut d = BytesMut::with_capacity(payload_owned.encoded_len(&()));
        let _ = payload_owned.encode_to(&mut d, &());
        let payload = d.freeze();

        header.param_len = param.len() as u16;
        header.payload_len = payload.len() as u16;

        S7Pdu {
            header,
            param,
            payload,
        }
    }

    /// Project a zero-copy structured view by parsing parameter and payload.
    pub fn as_ref_view(&self) -> Result<S7PduRef<'_>> {
        let (_, param) = parse_param_ref(self.header.pdu_type, &self.param)?;
        let (_, payload) = parse_payload_ref(self.header.pdu_type, &param, &self.payload)?;
        Ok(S7PduRef {
            header: self.header.clone(),
            param,
            payload,
        })
    }
}

impl WireEncode for S7Pdu {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        // Header(10) + param + payload + optional 2-byte error on Ack/AckData
        10 + self.param.len()
            + self.payload.len()
            + if matches!(self.header.pdu_type, S7PduType::Ack | S7PduType::AckData)
                && self.header.error_code.is_some()
            {
                2
            } else {
                0
            }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        self.header.encode_to(dst);
        if !self.param.is_empty() {
            dst.put_slice(&self.param);
        }
        if !self.payload.is_empty() {
            dst.put_slice(&self.payload);
        }
        Ok(())
    }
}

impl WireDecode for S7Pdu {
    type Error = Error;
    type Context = ();

    fn parse<'a>(
        input: &'a [u8],
        parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self)> {
        let (hdr, rest) = S7Header::parse(input)?;
        let need = (hdr.param_len as usize) + (hdr.payload_len as usize);
        if rest.len() < need {
            return Err(Error::ErrInvalidFrame);
        }
        let (param_bytes, tail) = rest.split_at(hdr.param_len as usize);
        let (payload_bytes, remain) = tail.split_at(hdr.payload_len as usize);
        Ok((
            remain,
            S7Pdu {
                header: hdr,
                param: Bytes::slice_ref(parent, param_bytes),
                payload: Bytes::slice_ref(parent, payload_bytes),
            },
        ))
    }
}

/// Zero-copy structured PDU view projected from `S7Pdu`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S7PduRef<'a> {
    pub header: S7Header,
    pub param: S7ParamRef<'a>,
    pub payload: S7PayloadRef<'a>,
}

impl<'a> TryFrom<&'a S7Pdu> for S7PduRef<'a> {
    type Error = Error;

    fn try_from(raw: &'a S7Pdu) -> Result<Self> {
        let (_, param) = parse_param_ref(raw.header.pdu_type, &raw.param)?;
        let (_, payload) = parse_payload_ref(raw.header.pdu_type, &param, &raw.payload)?;
        Ok(S7PduRef {
            header: raw.header.clone(),
            param,
            payload,
        })
    }
}
