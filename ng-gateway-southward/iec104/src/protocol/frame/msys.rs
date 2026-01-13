use super::{
    super::Error,
    asdu::{
        Asdu, CauseOfTransmission, CommonAddr, Identifier, InfoObjAddr, TypeID, VariableStruct,
    },
};
use anyhow::Result;
use bit_struct::*;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use bytes::Bytes;
use std::io::Cursor;

// 在监视方向系统信息的应用服务数据单元

// 初始化原因
bit_struct! {
    pub struct ObjectCOI(u8) {
        cause: u7, // 0: 电源上电, 1:手动复位, 2:远方复位
        flag: u1,  // 是否改变了当地参数
    }
}

// EndOfInitialization send a type identification [M_EI_NA_1],初始化结束,只有单个信息对象(SQ = 0)
// [M_EI_NA_1] See companion standard 101,subclass 7.3.3.1
// 传送原因(cot)用于
// 监视方向：
// <4> := 被初始化
async fn end_of_initialization(
    cot: CauseOfTransmission,
    ca: CommonAddr,
    ioa: InfoObjAddr,
    coi: ObjectCOI,
) -> Result<Asdu, Error> {
    let variable_struct = VariableStruct::new(u1!(0), u7!(1));
    let mut buf = vec![];
    buf.write_u24::<LittleEndian>(ioa.raw().value())?;
    buf.write_u8(coi.raw())?;

    Ok(Asdu {
        identifier: Identifier {
            type_id: TypeID::M_EI_NA_1,
            variable_struct,
            cot,
            orig_addr: 0,
            common_addr: ca,
        },
        raw: Bytes::from(buf),
    })
}

impl Asdu {
    // GetEndOfInitialization get GetEndOfInitialization for asdu when the identification [M_EI_NA_1]
    pub fn get_end_of_initialization(&mut self) -> Result<(InfoObjAddr, ObjectCOI)> {
        let mut rdr = Cursor::new(&self.raw);
        Ok((
            InfoObjAddr::try_from(
                u24::new(rdr.read_u24::<LittleEndian>()?)
                    .ok_or(anyhow::anyhow!("invalid u24 value for ioa"))?,
            )
            .map_err(|_| anyhow::anyhow!("invalid IOA"))?,
            ObjectCOI::try_from(rdr.read_u8()?).map_err(|_| anyhow::anyhow!("invalid COI"))?,
        ))
    }
}
