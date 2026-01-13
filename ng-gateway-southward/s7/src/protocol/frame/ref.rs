use super::{
    super::error::{Error, Result},
    iter::{
        UserDataParamIter, UserDataPayloadItemIter, UserDataTypedItemIter, VarPayloadDataItemIter,
        VarPayloadStatusItemIter, VarSpecIter,
    },
    types::{S7Function, S7PduType},
};
use nom::number::complete::{be_u16, u8 as nom_u8};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S7ParamRef<'a> {
    Job(S7JobParamRef<'a>),
    Ack(&'a [u8]),
    AckData(S7ParamAckDataRef<'a>),
    UserData(S7UserDataParamRef<'a>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S7JobParamRef<'a> {
    SetupCommunication(SetupParam),
    ReadVarRequest(ItemsParamRef<'a>),
    WriteVarRequest(ItemsParamRef<'a>),
    Unknown { function: u8, raw: &'a [u8] },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S7ParamAckDataRef<'a> {
    SetupCommunication(SetupParam),
    ReadVarResponse(ItemsCountParam),
    WriteVarResponse(ItemsCountParam),
    Unknown { function: u8, raw: &'a [u8] },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S7UserDataParamRef<'a> {
    CpuService(CpuServiceParam<'a>),
    Unknown { function: u8, raw: &'a [u8] },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupParam {
    pub reserved: u8,
    pub amq_caller: u16,
    pub amq_callee: u16,
    pub pdu_len: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemsParamRef<'a> {
    pub item_count: u8,
    pub raw_tail: &'a [u8],
}

impl<'a> ItemsParamRef<'a> {
    pub fn iter_specs(&self) -> VarSpecIter<'a> {
        VarSpecIter::new(self.item_count, self.raw_tail)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemsCountParam {
    pub item_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuServiceParam<'a> {
    pub item_count: u8,
    pub raw_tail: &'a [u8],
}

impl<'a> CpuServiceParam<'a> {
    pub fn iter_items(&self) -> UserDataParamIter<'a> {
        UserDataParamIter::new(self.item_count, self.raw_tail)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S7PayloadRef<'a> {
    Job(S7JobPayloadRef<'a>),
    AckData(S7AckDataPayloadRef<'a>),
    UserData(S7UserDataPayloadRef<'a>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S7JobPayloadRef<'a> {
    /// Payload of WriteVar request
    WriteVarRequest(WriteVarRequestPayloadRef<'a>),
    Unknown {
        function: u8,
        raw: &'a [u8],
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S7AckDataPayloadRef<'a> {
    /// Payload of ReadVar response
    ReadVarResponse(ReadVarResponsePayloadRef<'a>),
    /// Payload of WriteVar response
    WriteVarResponse(WriteVarResponsePayloadRef<'a>),
    Unknown {
        function: u8,
        raw: &'a [u8],
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S7UserDataPayloadRef<'a> {
    /// Payload for CPU Service user-data
    CpuService(UserDataPayloadRef<'a>),
    Unknown {
        function: u8,
        raw: &'a [u8],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadVarResponsePayloadRef<'a> {
    pub item_count: u8,
    pub raw_tail: &'a [u8],
}

impl<'a> ReadVarResponsePayloadRef<'a> {
    pub fn iter_items(&self) -> VarPayloadDataItemIter<'a> {
        VarPayloadDataItemIter::new(self.item_count, self.raw_tail)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteVarRequestPayloadRef<'a> {
    pub item_count: u8,
    pub raw_tail: &'a [u8],
}

impl<'a> WriteVarRequestPayloadRef<'a> {
    pub fn iter_items(&self) -> VarPayloadDataItemIter<'a> {
        VarPayloadDataItemIter::new(self.item_count, self.raw_tail)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteVarResponsePayloadRef<'a> {
    pub item_count: u8,
    pub raw_tail: &'a [u8],
}

impl<'a> WriteVarResponsePayloadRef<'a> {
    pub fn iter_items(&self) -> VarPayloadStatusItemIter<'a> {
        VarPayloadStatusItemIter::new(self.item_count, self.raw_tail)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserDataPayloadRef<'a> {
    pub item_count: u8,
    pub raw_tail: &'a [u8],
}

impl<'a> UserDataPayloadRef<'a> {
    pub fn iter_items(&self) -> UserDataPayloadItemIter<'a> {
        UserDataPayloadItemIter::new(self.item_count, self.raw_tail)
    }

    /// Zip-iterate parameter CPU function items with payload items and classify each payload
    /// according to PLC4X discriminators (cpu_function_group/type/subfunction + optional length).
    ///
    /// This provides a zero-copy, typed view over user-data payload. Each element contains both
    /// the discriminator and the parsed generic payload item fields as well as a high-level kind.
    pub fn iter_typed<'p>(&self, cpu_param: CpuServiceParam<'p>) -> UserDataTypedItemIter<'a, 'p> {
        UserDataTypedItemIter::new(cpu_param.item_count, cpu_param.raw_tail, self.raw_tail)
    }
}

pub fn parse_param_ref<'a>(pdu: S7PduType, input: &'a [u8]) -> Result<(&'a [u8], S7ParamRef<'a>)> {
    let (i, func) =
        nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
    match pdu {
        S7PduType::Job => parse_job_param(func, i).map(|(r, v)| (r, S7ParamRef::Job(v))),
        S7PduType::AckData => {
            parse_ack_data_param(func, i).map(|(r, v)| (r, S7ParamRef::AckData(v)))
        }
        S7PduType::UserData => {
            parse_user_data_param(func, i).map(|(r, v)| (r, S7ParamRef::UserData(v)))
        }
        S7PduType::Ack => Ok((i, S7ParamRef::Ack(input))),
    }
}

fn parse_ack_data_param<'a>(
    func: u8,
    input: &'a [u8],
) -> Result<(&'a [u8], S7ParamAckDataRef<'a>)> {
    let func: S7Function = func.try_into().map_err(|_| Error::ErrInvalidFrame)?;
    match func {
        S7Function::SetupCommunication => {
            let (i, _zero) =
                nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
            let (i, amq_caller) =
                be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
            let (i, amq_callee) =
                be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
            let (i, pdu_len) =
                be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
            Ok((
                i,
                S7ParamAckDataRef::SetupCommunication(SetupParam {
                    reserved: 0,
                    amq_caller,
                    amq_callee,
                    pdu_len,
                }),
            ))
        }
        S7Function::ReadVar => {
            let (i, item_count) =
                nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
            Ok((
                i,
                S7ParamAckDataRef::ReadVarResponse(ItemsCountParam { item_count }),
            ))
        }
        S7Function::WriteVar => {
            let (i, item_count) =
                nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
            Ok((
                i,
                S7ParamAckDataRef::WriteVarResponse(ItemsCountParam { item_count }),
            ))
        }
        _ => Ok((
            input,
            S7ParamAckDataRef::Unknown {
                function: func as u8,
                raw: input,
            },
        )),
    }
}

fn parse_user_data_param<'a>(
    func: u8,
    input: &'a [u8],
) -> Result<(&'a [u8], S7UserDataParamRef<'a>)> {
    let func: S7Function = func.try_into().map_err(|_| Error::ErrInvalidFrame)?;
    match func {
        S7Function::CpuService => {
            // According to PLC4X, S7ParameterUserData starts with numItems then items...
            let (i, num_items) =
                nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
            Ok((
                i,
                S7UserDataParamRef::CpuService(CpuServiceParam {
                    item_count: num_items,
                    raw_tail: i,
                }),
            ))
        }
        _ => Ok((
            input,
            S7UserDataParamRef::Unknown {
                function: func as u8,
                raw: input,
            },
        )),
    }
}

fn parse_job_param<'a>(func: u8, input: &'a [u8]) -> Result<(&'a [u8], S7JobParamRef<'a>)> {
    let func: S7Function = func.try_into().map_err(|_| Error::ErrInvalidFrame)?;
    match func {
        S7Function::SetupCommunication => {
            let (i, _zero) =
                nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
            let (i, amq_caller) =
                be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
            let (i, amq_callee) =
                be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
            let (i, pdu_len) =
                be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
            Ok((
                i,
                S7JobParamRef::SetupCommunication(SetupParam {
                    reserved: 0,
                    amq_caller,
                    amq_callee,
                    pdu_len,
                }),
            ))
        }
        S7Function::ReadVar => {
            let (i, item_count) =
                nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
            Ok((
                i,
                S7JobParamRef::ReadVarRequest(ItemsParamRef {
                    item_count,
                    raw_tail: i,
                }),
            ))
        }
        S7Function::WriteVar => {
            let (i, item_count) =
                nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
            Ok((
                i,
                S7JobParamRef::WriteVarRequest(ItemsParamRef {
                    item_count,
                    raw_tail: i,
                }),
            ))
        }
        _ => Ok((
            input,
            S7JobParamRef::Unknown {
                function: func as u8,
                raw: input,
            },
        )),
    }
}

pub fn parse_payload_ref<'a>(
    pdu: S7PduType,
    param: &S7ParamRef<'a>,
    input: &'a [u8],
) -> Result<(&'a [u8], S7PayloadRef<'a>)> {
    match (pdu, param) {
        (S7PduType::Job, S7ParamRef::Job(job)) => match job {
            S7JobParamRef::WriteVarRequest(ItemsParamRef { item_count, .. }) => {
                // Walk the items to compute the end pointer
                let mut r = input;
                let iter = VarPayloadDataItemIter::new(*item_count, r);
                for (item, next) in iter {
                    let _ = item;
                    r = next;
                }
                Ok((
                    r,
                    S7PayloadRef::Job(S7JobPayloadRef::WriteVarRequest(
                        WriteVarRequestPayloadRef {
                            item_count: *item_count,
                            raw_tail: input,
                        },
                    )),
                ))
            }
            _ => Ok((
                input,
                S7PayloadRef::Job(S7JobPayloadRef::Unknown {
                    function: match job {
                        S7JobParamRef::SetupCommunication(_) => {
                            S7Function::SetupCommunication as u8
                        }
                        S7JobParamRef::ReadVarRequest(_) => S7Function::ReadVar as u8,
                        S7JobParamRef::WriteVarRequest(_) => S7Function::WriteVar as u8,
                        S7JobParamRef::Unknown { function, .. } => *function,
                    },
                    raw: input,
                }),
            )),
        },
        (S7PduType::AckData, S7ParamRef::AckData(ack)) => match ack {
            S7ParamAckDataRef::ReadVarResponse(ItemsCountParam { item_count }) => {
                let mut r = input;
                let iter = VarPayloadDataItemIter::new(*item_count, r);
                for (item, next) in iter {
                    let _ = item;
                    r = next;
                }
                Ok((
                    r,
                    S7PayloadRef::AckData(S7AckDataPayloadRef::ReadVarResponse(
                        ReadVarResponsePayloadRef {
                            item_count: *item_count,
                            raw_tail: input,
                        },
                    )),
                ))
            }
            S7ParamAckDataRef::WriteVarResponse(ItemsCountParam { item_count }) => {
                let mut r = input;
                let iter = VarPayloadStatusItemIter::new(*item_count, r);
                for (_item, next) in iter {
                    r = next;
                }
                Ok((
                    r,
                    S7PayloadRef::AckData(S7AckDataPayloadRef::WriteVarResponse(
                        WriteVarResponsePayloadRef {
                            item_count: *item_count,
                            raw_tail: input,
                        },
                    )),
                ))
            }
            _ => Ok((
                input,
                S7PayloadRef::AckData(S7AckDataPayloadRef::Unknown {
                    function: match ack {
                        S7ParamAckDataRef::SetupCommunication(_) => {
                            S7Function::SetupCommunication as u8
                        }
                        S7ParamAckDataRef::ReadVarResponse(_) => S7Function::ReadVar as u8,
                        S7ParamAckDataRef::WriteVarResponse(_) => S7Function::WriteVar as u8,
                        S7ParamAckDataRef::Unknown { function, .. } => *function,
                    },
                    raw: input,
                }),
            )),
        },
        (S7PduType::UserData, S7ParamRef::UserData(ud)) => match ud {
            S7UserDataParamRef::CpuService(CpuServiceParam { item_count, .. }) => {
                let mut r = input;
                let iter = UserDataPayloadItemIter::new(*item_count, r);
                for (_item, next) in iter {
                    r = next;
                }
                Ok((
                    r,
                    S7PayloadRef::UserData(S7UserDataPayloadRef::CpuService(UserDataPayloadRef {
                        item_count: *item_count,
                        raw_tail: input,
                    })),
                ))
            }
            S7UserDataParamRef::Unknown { function, .. } => Ok((
                input,
                S7PayloadRef::UserData(S7UserDataPayloadRef::Unknown {
                    function: *function,
                    raw: input,
                }),
            )),
        },
        // Any other combination: leave bytes untouched and mark unknown
        (pdu_kind, _) => Ok((
            input,
            match pdu_kind {
                S7PduType::Job => S7PayloadRef::Job(S7JobPayloadRef::Unknown {
                    function: 0,
                    raw: input,
                }),
                S7PduType::AckData => S7PayloadRef::AckData(S7AckDataPayloadRef::Unknown {
                    function: 0,
                    raw: input,
                }),
                S7PduType::UserData => S7PayloadRef::UserData(S7UserDataPayloadRef::Unknown {
                    function: 0,
                    raw: input,
                }),
                S7PduType::Ack => S7PayloadRef::AckData(S7AckDataPayloadRef::Unknown {
                    function: 0,
                    raw: input,
                }),
            },
        )),
    }
}
