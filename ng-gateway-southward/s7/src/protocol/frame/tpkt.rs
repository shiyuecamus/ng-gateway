use super::{
    super::error::{Error, Result},
    WireDecode, WireEncode,
};
use bytes::{BufMut, Bytes};

/// TPKT (RFC1006) header: 4 bytes
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Tpkt {
    /// Always 0x03 for RFC1006
    pub version: u8,
    /// Reserved, always 0x00
    pub reserved: u8,
    /// Total length including this 4-byte header
    pub length: u16,
}

impl Tpkt {
    /// Build a TPKT header from payload length (payload includes COTP bytes).
    pub fn with_payload_len(payload_len: usize) -> Self {
        let total = 4u16.saturating_add(payload_len as u16);
        Self {
            version: 0x03,
            reserved: 0x00,
            length: total,
        }
    }

    /// Encode only the TPKT header for a total frame length (header + payload).
    /// The `total_len` must include the 4-byte TPKT header.
    pub fn encode_header_to<B: BufMut>(total_len: usize, dst: &mut B) {
        dst.put_u8(0x03);
        dst.put_u8(0x00);
        dst.put_u16(total_len as u16);
    }
}

impl WireEncode for Tpkt {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        4
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        dst.put_u8(self.version);
        dst.put_u8(self.reserved);
        dst.put_u16(self.length);
        Ok(())
    }
}

impl WireDecode for Tpkt {
    type Error = Error;
    type Context = ();

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self)> {
        if input.len() < 4 {
            return Err(Error::InsufficientData {
                needed: 4,
                available: input.len(),
            });
        }
        let hdr = &input[..4];
        let version = hdr[0];
        let reserved = hdr[1];
        if version != 0x03 || reserved != 0x00 {
            return Err(Error::ErrInvalidFrame);
        }
        let length = u16::from_be_bytes([hdr[2], hdr[3]]) as usize;
        if length < 4 || length > input.len() {
            return Err(Error::ErrInvalidFrame);
        }
        let rest = &input[length..];
        Ok((
            rest,
            Tpkt {
                version,
                reserved,
                length: length as u16,
            },
        ))
    }
}
