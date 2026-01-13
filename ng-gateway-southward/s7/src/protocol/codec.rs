use super::frame::{Cotp, S7AppBody, S7Message, S7Pdu, Tpkt};
use bytes::BufMut;
use bytes::BytesMut;
use ng_gateway_sdk::{WireDecode, WireEncode};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

/// Unified codec that handles RFC1006(TPKT) + COTP + S7
#[derive(Debug, Default, Clone)]
pub struct Codec;

impl Decoder for Codec {
    type Item = S7Message;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Ensure we have at least a TPKT header
        if src.len() < 4 {
            return Ok(None);
        }
        if src[0] != 0x03 || src[1] != 0x00 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid TPKT header",
            ));
        }
        let length = u16::from_be_bytes([src[2], src[3]]) as usize;
        if length < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid TPKT length",
            ));
        }
        if src.len() < length {
            return Ok(None);
        }

        // Split the complete TPKT frame and decode COTP on that slice
        let frame = src.split_to(length).freeze();
        let tpkt = Tpkt {
            version: frame[0],
            reserved: frame[1],
            length: u16::from_be_bytes([frame[2], frame[3]]),
        };
        let cotp_payload = &frame[4..];
        let (_r_cotp, cotp) = Cotp::parse(cotp_payload, &frame, &())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("cotp: {e:?}")))?;
        let mut app: Option<S7AppBody> = None;
        // Compute payload: the COTP encoded_len equals the header bytes (LI + type + body), payload is rest
        let cotp_li = cotp.encoded_len(&());
        let frame_len = frame.len();
        let after_cotp = 4 + cotp_li;
        if frame_len >= after_cotp {
            let user = frame.slice(after_cotp..frame_len);
            if matches!(cotp, Cotp::D(_)) && !user.is_empty() {
                if let Cotp::D(params) = &cotp {
                    if params.eot {
                        if let Ok((_remain, pdu)) = <S7Pdu as WireDecode>::parse(&user, &frame, &())
                        {
                            app = Some(S7AppBody::Parsed(pdu));
                        } else {
                            app = Some(S7AppBody::Segmented(user));
                        }
                    } else {
                        app = Some(S7AppBody::Segmented(user));
                    }
                }
            }
        }
        Ok(Some(S7Message { tpkt, cotp, app }))
    }
}

impl Encoder<S7Message> for Codec {
    type Error = io::Error;

    fn encode(&mut self, item: S7Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // Encode TPKT header + COTP TPDU + optional S7 payload (for Data TPDU)
        let tpkt_len = item.tpkt.encoded_len(&());
        let cotp_len = item.cotp.encoded_len(&());
        let s7_len = match (&item.cotp, &item.app) {
            (Cotp::D(_), Some(S7AppBody::Segmented(b))) => b.len(),
            (Cotp::D(_), Some(S7AppBody::Parsed(p))) => p.encoded_len(&()),
            _ => 0,
        };
        let total_len = tpkt_len + cotp_len + s7_len;
        dst.reserve(total_len);

        Tpkt::encode_header_to(total_len, dst);

        let _ = item.cotp.encode_to(dst, &());

        // Append S7 layer if present
        match (item.cotp, item.app) {
            (Cotp::D(_), Some(S7AppBody::Segmented(b))) => {
                if !b.is_empty() {
                    dst.put_slice(&b);
                }
            }
            (Cotp::D(_), Some(S7AppBody::Parsed(p))) => {
                let _ = p.encode_to(dst, &());
            }
            _ => {}
        }
        Ok(())
    }
}
