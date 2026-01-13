use anyhow::{anyhow, Context, Result};
use bytes::{BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use super::frame::{
    apci::{Apci, ApciKind, APCI_FIELD_SIZE, APDU_SIZE_MAX, START_FRAME},
    Apdu,
};

#[derive(Debug, PartialEq, Default)]
pub struct Codec;

impl Encoder<Apdu> for Codec {
    type Error = anyhow::Error;

    fn encode(&mut self, apdu: Apdu, buf: &mut BytesMut) -> Result<()> {
        let apci = apdu.apci;
        let asdu = apdu.asdu;

        // Reserve exact capacity to avoid repeated reallocations and copies
        // Total wire length = apci.apdu_length + 2 (start + length fields)
        let expected_len = apci.apdu_length as usize + 2;
        buf.reserve(expected_len);

        buf.put_u8(apci.start);
        buf.put_u8(apci.apdu_length);
        buf.put_u8(apci.ctrl1);
        buf.put_u8(apci.ctrl2);
        buf.put_u8(apci.ctrl3);
        buf.put_u8(apci.ctrl4);

        if let Some(asdu) = asdu {
            // Directly write ASDU identifier and raw payload to the output buffer
            // to avoid intermediate allocations and copies.
            buf.put_u8(asdu.identifier.type_id as u8);
            buf.put_u8(asdu.identifier.variable_struct.raw());
            buf.put_u8(asdu.identifier.cot.raw());
            buf.put_u8(asdu.identifier.orig_addr);
            buf.put_u16_le(asdu.identifier.common_addr);
            buf.extend_from_slice(&asdu.raw);
        }

        Ok(())
    }
}

impl Decoder for Codec {
    type Item = Apdu;

    type Error = anyhow::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>> {
        if buf.len() < APCI_FIELD_SIZE {
            return Ok(None);
        }
        let len = buf[1] as usize + 2;
        if !(APCI_FIELD_SIZE..=APDU_SIZE_MAX).contains(&len) {
            return Err(anyhow!("Invalid APDU length:{}", len));
        }

        if buf.len() < len {
            return Ok(None);
        }
        let apci_data = buf.split_to(APCI_FIELD_SIZE);
        if apci_data[0] != START_FRAME {
            return Err(anyhow!("Invalid start frame:{}", apci_data[0]));
        }
        let apci = Apci {
            start: apci_data[0],
            apdu_length: apci_data[1],
            ctrl1: apci_data[2],
            ctrl2: apci_data[3],
            ctrl3: apci_data[4],
            ctrl4: apci_data[5],
        };

        let apci_kind = apci.into();

        match apci_kind {
            ApciKind::I(_) => {
                let asdu_data = buf.split_to(len - APCI_FIELD_SIZE).freeze();
                match asdu_data.clone().try_into() {
                    Ok(asdu) => Ok(Some(Apdu {
                        apci,
                        asdu: Some(asdu),
                    })),
                    Err(e) => {
                        let asdu_hex: String =
                            asdu_data.iter().map(|b| format!("{:02X} ", b)).collect();
                        tracing::warn!(
                            error = %e,
                            apdu_length = apci.apdu_length,
                            ctrl1 = apci.ctrl1,
                            ctrl2 = apci.ctrl2,
                            ctrl3 = apci.ctrl3,
                            ctrl4 = apci.ctrl4,
                            asdu_hex = %asdu_hex,
                            "ASDU decode failed; dropping payload"
                        );
                        Ok(Some(Apdu { apci, asdu: None }))
                    }
                }
            }
            _ => Ok(Some(Apdu { apci, asdu: None })),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::super::frame::{
        apci::U_STARTDT_ACTIVE,
        asdu::{Asdu, CauseOfTransmission, Identifier, TypeID, VariableStruct, IDENTIFIER_SIZE},
    };
    use super::*;

    #[test]
    fn decode_iapci() {
        let mut codec = Codec;
        let mut buf = BytesMut::from(&[START_FRAME, 0x04, 0x02, 0x00, 0x03, 0x00][..]);
        let apdu = codec.decode(&mut buf).unwrap().unwrap();
        let apci_kind = apdu.apci.into();
        match apci_kind {
            ApciKind::I(apci) => {
                assert_eq!(apci.send_sn, 0x01);
                assert_eq!(apci.rcv_sn, 0x01);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn decode_sapci() {
        let mut codec = Codec;
        let mut buf = BytesMut::from(&[START_FRAME, 0x04, 0x01, 0x00, 0x02, 0x00][..]);
        let apdu = codec.decode(&mut buf).unwrap().unwrap();
        let apci_kind = apdu.apci.into();
        match apci_kind {
            ApciKind::S(apci) => {
                assert_eq!(apci.rcv_sn, 0x01);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn decode_uapci() {
        let mut codec = Codec;
        let mut buf = BytesMut::from(&[START_FRAME, 0x04, 0x07, 0x00, 0x00, 0x00][..]);
        let apdu = codec.decode(&mut buf).unwrap().unwrap();
        let apci_kind = apdu.apci.into();
        match apci_kind {
            ApciKind::U(apci) => {
                assert_eq!(apci.function, U_STARTDT_ACTIVE);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn encode_iapci() {
        let mut codec = Codec;
        let apdu = Apdu {
            apci: Apci {
                start: START_FRAME,
                apdu_length: 0x04 + IDENTIFIER_SIZE as u8 + 8,
                ctrl1: 0x02,
                ctrl2: 0x00,
                ctrl3: 0x03,
                ctrl4: 0x00,
            },
            asdu: Some(Asdu {
                identifier: Identifier {
                    type_id: TypeID::M_SP_NA_1,
                    variable_struct: VariableStruct::try_from(0x02).unwrap(),
                    cot: CauseOfTransmission::try_from(0x06).unwrap(),
                    orig_addr: 0,
                    common_addr: 0,
                },
                raw: Bytes::from_static(&[0x01, 0x00, 0x00, 0x11, 0x02, 0x00, 0x00, 0x10]),
            }),
        };
        let want = [
            START_FRAME,
            0x04 + IDENTIFIER_SIZE as u8 + 8,
            0x02,
            0x00,
            0x03,
            0x00,
            0x01,
            0x02,
            0x06,
            0x00,
            0x00,
            0x00,
            0x01,
            0x00,
            0x00,
            0x11,
            0x02,
            0x00,
            0x00,
            0x10,
        ];

        let mut buf = BytesMut::with_capacity(APDU_SIZE_MAX);
        codec.encode(apdu, &mut buf).unwrap();
        assert_eq!(buf.as_ref(), &want[..]);
    }
}
