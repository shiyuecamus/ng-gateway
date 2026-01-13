use super::{
    super::error::{Error, Result},
    cotp_param::{
        CotpCcParams, CotpCrParams, CotpDataParams, CotpDcParams, CotpDrParams, CotpReParams,
    },
    types::CotpType,
    WireDecode, WireEncode,
};
use bytes::{BufMut, Bytes};

/// COTP (subset sufficient for ISO-on-TCP + S7) - fully structured design
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cotp {
    /// Connection Request
    Cr(CotpCrParams),
    /// Connection Confirm
    Cc(CotpCcParams),
    /// Disconnection Request
    Dr(CotpDrParams),
    /// Disconnection Confirm
    Dc(CotpDcParams),
    /// Reject
    Re(CotpReParams),
    /// Data TPDU
    D(CotpDataParams),
}

impl Cotp {
    /// Get source TSAP if available
    pub fn src_tsap(&self) -> Option<u16> {
        match self {
            Cotp::Cr(params) => Some(params.src_tsap),
            Cotp::Cc(params) => Some(params.src_tsap),
            _ => None,
        }
    }

    /// Get destination TSAP if available
    pub fn dst_tsap(&self) -> Option<u16> {
        match self {
            Cotp::Cr(params) => Some(params.dst_tsap),
            Cotp::Cc(params) => Some(params.dst_tsap),
            _ => None,
        }
    }

    /// Get source reference
    pub fn src_ref(&self) -> Option<u16> {
        match self {
            Cotp::Cr(params) => Some(params.src_ref),
            Cotp::Cc(params) => Some(params.src_ref),
            Cotp::Dr(params) => Some(params.src_ref),
            Cotp::Dc(params) => Some(params.src_ref),
            Cotp::Re(_params) => None,
            Cotp::D { .. } => None,
        }
    }

    /// Get destination reference
    pub fn dst_ref(&self) -> Option<u16> {
        match self {
            Cotp::Cr(params) => Some(params.dst_ref),
            Cotp::Cc(params) => Some(params.dst_ref),
            Cotp::Dr(params) => Some(params.dst_ref),
            Cotp::Dc(params) => Some(params.dst_ref),
            Cotp::Re(params) => Some(params.dst_ref),
            Cotp::D { .. } => None,
        }
    }

    /// Get TPDU size if available
    pub fn tpdu_size(&self) -> Option<u8> {
        match self {
            Cotp::Cr(params) => Some(params.tpdu_size),
            Cotp::Cc(params) => Some(params.tpdu_size),
            _ => None,
        }
    }

    /// Convenience: get TPDU size in bytes if the PDU carries it
    pub fn tpdu_size_bytes(&self) -> Option<usize> {
        match self {
            Cotp::Cr(params) => params.tpdu_size_bytes(),
            Cotp::Cc(params) => params.tpdu_size_bytes(),
            _ => None,
        }
    }
}

// Wire traits
impl WireEncode for Cotp {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        match self {
            Cotp::Cr(p) => 1 + (1 + p.encoded_len(ctx)),
            Cotp::Cc(p) => 1 + (1 + p.encoded_len(ctx)),
            Cotp::Dr(p) => 1 + (1 + p.encoded_len(ctx)),
            Cotp::Dc(p) => 1 + (1 + p.encoded_len(ctx)),
            Cotp::Re(p) => 1 + (1 + p.encoded_len(ctx)),
            Cotp::D(params) => 1 + (1 + params.encoded_len(ctx)),
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<()> {
        match self {
            Cotp::Cr(params) => {
                let li = (1 + params.encoded_len(ctx)) as u8;
                dst.put_u8(li);
                dst.put_u8(CotpType::Cr as u8);
                params.encode_to(dst, ctx)?;
            }
            Cotp::Cc(params) => {
                let li = (1 + params.encoded_len(ctx)) as u8;
                dst.put_u8(li);
                dst.put_u8(CotpType::Cc as u8);
                params.encode_to(dst, ctx)?;
            }
            Cotp::Dr(params) => {
                let li = (1 + params.encoded_len(ctx)) as u8;
                dst.put_u8(li);
                dst.put_u8(CotpType::Dr as u8);
                params.encode_to(dst, ctx)?;
            }
            Cotp::Dc(params) => {
                let li = (1 + params.encoded_len(ctx)) as u8;
                dst.put_u8(li);
                dst.put_u8(CotpType::Dc as u8);
                params.encode_to(dst, ctx)?;
            }
            Cotp::Re(params) => {
                let li = (1 + params.encoded_len(ctx)) as u8;
                dst.put_u8(li);
                dst.put_u8(CotpType::Re as u8);
                params.encode_to(dst, ctx)?;
            }
            Cotp::D(params) => {
                let li = (1 + params.encoded_len(ctx)) as u8; // type + eot_nr + payload
                dst.put_u8(li);
                dst.put_u8(CotpType::D as u8);
                params.encode_to(dst, ctx)?;
            }
        }
        Ok(())
    }
}

impl WireDecode for Cotp {
    type Error = Error;
    type Context = ();

    // Parse one COTP from the provided slice.
    ///
    /// The function consumes exactly the bytes indicated by the COTP LI field (plus the LI byte itself).
    /// For Data, the user payload is the remaining bytes after the header and is returned as a slice reference.
    /// The returned rest slice points to any trailing bytes after the parsed COTP (normally empty for RFC1006).
    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self)> {
        if input.len() < 2 {
            return Err(Error::InsufficientData {
                needed: 2,
                available: input.len(),
            });
        }
        let li = input[0] as usize; // Length indicator excludes this LI byte
        let total = 1usize + li;
        if total > input.len() || li < 1 {
            return Err(Error::ProtocolViolation {
                context: "invalid COTP LI",
            });
        }
        let tpdu_type = input[1];
        let rest = &input[total..];
        let body = &input[2..total];

        let pdu_type = tpdu_type.try_into().map_err(|_| Error::ProtocolViolation {
            context: "unknown COTP PDU type",
        })?;
        match pdu_type {
            CotpType::D => Ok((rest, Cotp::D(CotpDataParams::parse_body(body)?))),
            CotpType::Cr => Ok((rest, Cotp::Cr(CotpCrParams::parse_body(body)?))),
            CotpType::Cc => Ok((rest, Cotp::Cc(CotpCcParams::parse_body(body)?))),
            CotpType::Dr => Ok((rest, Cotp::Dr(CotpDrParams::parse_body(body)?))),
            CotpType::Dc => Ok((rest, Cotp::Dc(CotpDcParams::parse_body(body)?))),
            CotpType::Re => Ok((rest, Cotp::Re(CotpReParams::parse_body(body)?))),
        }
    }
}
