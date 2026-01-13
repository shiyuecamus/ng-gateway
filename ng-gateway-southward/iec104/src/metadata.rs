use crate::protocol::frame::asdu::TypeID;
use ng_gateway_sdk::{
    ui_text, DriverSchemas, EnumItem, Field, Group, Node, RuleValue, Rules, UiDataType, UiProps,
};
use serde_json::json;

/// Build static metadata once to be embedded as JSON for the gateway UI/config.
pub(super) fn build_metadata() -> DriverSchemas {
    DriverSchemas {
        channel: build_channel_nodes(),
        device: build_device_nodes(),
        point: build_point_nodes(),
        action: build_action_nodes(),
    }
}

/// Build channel-level configuration nodes for the IEC104 driver.
fn build_channel_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "host".into(),
            label: ui_text!(en = "Host", zh = "主机"),
            data_type: UiDataType::String,
            default_value: None,
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Host is required", zh = "主机是必填项")),
                }),
                // Hostname (RFC-1123) OR IPv4
                pattern: Some(RuleValue::WithMessage {
                    value: "^(?:(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)(?:\\.(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?))*|(?:(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)\\.){3}(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d))$"
                        .to_string(),
                    message: Some(ui_text!(
                        en = "Enter a valid IPv4 address or hostname (no schema/port)",
                        zh = "请输入有效的 IPv4 或主机名（不含协议和端口）"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "port".into(),
            label: ui_text!(en = "Port", zh = "端口"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(2404)),
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Port is required", zh = "端口是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: Some(ui_text!(en = "Port must be > 0", zh = "端口必须大于0")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 65535.0,
                    message: Some(ui_text!(
                        en = "Port must be < 65536",
                        zh = "端口必须小于65536"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Group(Group{
            id: "advanced".into(),
            label: ui_text!(en = "Advanced Settings", zh = "高级设置"),
            description: None,
            collapsible: true,
            order: Some(3),
            children: vec![
                // Timeouts
                Node::Field(Box::new(Field {
                    path: "t0Ms".into(),
                    label: ui_text!(en = "t0", zh = "t0"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(10_000)),
                    order: Some(3),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 100.0,
                            message: Some(ui_text!(en = "Min 100ms", zh = "至少 100ms")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "t1Ms".into(),
                    label: ui_text!(en = "t1", zh = "t1"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(15_000)),
                    order: Some(4),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 100.0,
                            message: Some(ui_text!(en = "Min 100ms", zh = "至少 100ms")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "t2Ms".into(),
                    label: ui_text!(en = "t2", zh = "t2"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(10_000)),
                    order: Some(5),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 100.0,
                            message: Some(ui_text!(en = "Min 100ms", zh = "至少 100ms")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "t3Ms".into(),
                    label: ui_text!(en = "t3", zh = "t3"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(20_000)),
                    order: Some(6),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 100.0,
                            message: Some(ui_text!(en = "Min 100ms", zh = "至少 100ms")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                // Windows / thresholds
                Node::Field(Box::new(Field {
                    path: "kWindow".into(),
                    label: ui_text!(en = "K Window", zh = "K 窗口"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(12)),
                    order: Some(7),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 1.0,
                            message: Some(ui_text!(en = "Min 1", zh = "最小为 1")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "wThreshold".into(),
                    label: ui_text!(en = "W Threshold", zh = "W 阈值"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(8)),
                    order: Some(8),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 1.0,
                            message: Some(ui_text!(en = "Min 1", zh = "最小为 1")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                // Booleans and capacities
                Node::Field(Box::new(Field {
                    path: "tcpNodelay".into(),
                    label: ui_text!(en = "TCP NoDelay", zh = "禁用Nagle"),
                    data_type: UiDataType::Boolean,
                    default_value: Some(json!(true)),
                    order: Some(9),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        ..Default::default()
                    }),
                    rules: None,
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "sendQueueCapacity".into(),
                    label: ui_text!(en = "Send Queue Capacity", zh = "发送队列容量"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(1024)),
                    order: Some(10),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 1.0,
                            message: Some(ui_text!(en = "Min 1", zh = "最小为 1")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "maxPendingAsduBytes".into(),
                    label: ui_text!(
                        en = "Max Pending ASDU Bytes",
                        zh = "待处理字节上限"
                    ),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(1024 * 1024)),
                    order: Some(11),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 1024.0,
                            message: Some(ui_text!(en = "Min 1024", zh = "至少 1024")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "discardLowPriorityWhenWindowFull".into(),
                    label: ui_text!(
                        en = "Discard Low Priority When Window Full",
                        zh = "满窗口丢弃低优先级"
                    ),
                    data_type: UiDataType::Boolean,
                    default_value: Some(json!(true)),
                    order: Some(12),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        ..Default::default()
                    }),
                    rules: None,
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "mergeLowPriority".into(),
                    label: ui_text!(en = "Merge Low Priority", zh = "合并低优先级"),
                    data_type: UiDataType::Boolean,
                    default_value: Some(json!(true)),
                    order: Some(13),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        ..Default::default()
                    }),
                    rules: None,
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "lowPrioFlushMaxAgeMs".into(),
                    label: ui_text!(
                        en = "Low Prio Flush Max Age",
                        zh = "低优先级刷新最大延迟"
                    ),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(2000)),
                    order: Some(14),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 0.0,
                            message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "startupQoi".into(),
                    label: ui_text!(en = "Startup QOI", zh = "启动QOI"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(20)),
                    order: Some(17),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 0.0,
                            message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                        }),
                        max: Some(RuleValue::WithMessage {
                            value: 255.0,
                            message: Some(ui_text!(en = "Max 255", zh = "最大为 255")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "startupQcc".into(),
                    label: ui_text!(en = "Startup QCC", zh = "启动QCC"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(5)),
                    order: Some(18),
                    ui: Some(UiProps {
                        col_span: Some(1),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 0.0,
                            message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                        }),
                        max: Some(RuleValue::WithMessage {
                            value: 255.0,
                            message: Some(ui_text!(en = "Max 255", zh = "最大为 255")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
            ],
        }),
    ]
}

/// Build device-level configuration nodes for the IEC104 driver.
fn build_device_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "ca".into(),
        label: ui_text!(en = "Common Address", zh = "公共地址"),
        data_type: UiDataType::Integer,
        default_value: None,
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: Some(ui_text!(en = "CA is required", zh = "公共地址是必填项")),
            }),
            min: Some(RuleValue::WithMessage {
                value: 1.0,
                message: Some(ui_text!(en = "Min 1", zh = "最小为 1")),
            }),
            max: Some(RuleValue::WithMessage {
                value: 65535.0,
                message: Some(ui_text!(en = "Max 65535", zh = "最大为 65535")),
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}

/// Build point-level configuration nodes for the IEC104 driver.
fn build_point_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "ioa".into(),
            label: ui_text!(en = "IOA", zh = "IOA"),
            data_type: UiDataType::Integer,
            default_value: None,
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "IOA is required", zh = "IOA 是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 65535.0,
                    message: Some(ui_text!(en = "Max 65535", zh = "最大为 65535")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "typeId".into(),
            label: ui_text!(en = "ASDU Type ID", zh = "ASDU类型"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(TypeID::M_SP_NA_1 as u8),
                        label: ui_text!(en = "Single Point No Time", zh = "单点无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_SP_TA_1 as u8),
                        label: ui_text!(en = "Single Point With Time", zh = "单点带时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_SP_TB_1 as u8),
                        label: ui_text!(
                            en = "Single Point With CP56Time2a Time",
                            zh = "单点信息带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::M_DP_NA_1 as u8),
                        label: ui_text!(en = "Double Point No Time", zh = "双点无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_DP_TA_1 as u8),
                        label: ui_text!(en = "Double Point With Time", zh = "双点带时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_DP_TB_1 as u8),
                        label: ui_text!(
                            en = "Double Point With CP56Time2a Time",
                            zh = "双点信息带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ST_NA_1 as u8),
                        label: ui_text!(en = "Step Position No Time", zh = "步进位置无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ST_TA_1 as u8),
                        label: ui_text!(en = "Step Position With Time", zh = "步进位置带时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ST_TB_1 as u8),
                        label: ui_text!(
                            en = "Step Position With CP56Time2a Time",
                            zh = "步进位置信息带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::M_BO_NA_1 as u8),
                        label: ui_text!(en = "32 Bit String No Time", zh = "32比特串无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_BO_TA_1 as u8),
                        label: ui_text!(en = "32 Bit String With Time", zh = "32比特串带时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_BO_TB_1 as u8),
                        label: ui_text!(
                            en = "32 Bit String With CP56Time2a Time",
                            zh = "32比特串信息带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_NA_1 as u8),
                        label: ui_text!(en = "Measured Value No Time", zh = "归一化值无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_TA_1 as u8),
                        label: ui_text!(en = "Measured Value With Time", zh = "归一化值带时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_TD_1 as u8),
                        label: ui_text!(
                            en = "Measured Value With CP56Time2a Time",
                            zh = "归一化值带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_ND_1 as u8),
                        label: ui_text!(
                            en = "Measured Value No Time (No Quality Description)",
                            zh = "归一化值无时标（无品质描述）"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_NB_1 as u8),
                        label: ui_text!(en = "Scaled Value No Time", zh = "标度化值无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_TB_1 as u8),
                        label: ui_text!(en = "Scaled Value With Time", zh = "标度化值带时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_TE_1 as u8),
                        label: ui_text!(
                            en = "Scaled Value With CP56Time2a Time",
                            zh = "标度化值带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_NC_1 as u8),
                        label: ui_text!(en = "Short Float No Time", zh = "短浮点数无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_TC_1 as u8),
                        label: ui_text!(en = "Short Float With Time", zh = "短浮点数带时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_ME_TF_1 as u8),
                        label: ui_text!(
                            en = "Short Float With CP56Time2a Time",
                            zh = "短浮点数带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::M_IT_NA_1 as u8),
                        label: ui_text!(en = "Accumulated Value No Time", zh = "累计量无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_IT_TA_1 as u8),
                        label: ui_text!(en = "Accumulated Value With Time", zh = "累计量带时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::M_IT_TB_1 as u8),
                        label: ui_text!(
                            en = "Accumulated Value With CP56Time2a Time",
                            zh = "累计量带CP56Time2a时标"
                        ),
                    },
                ],
            },
            default_value: None,
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Type ID is required",
                        zh = "类型 ID 是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}

/// Build action-level configuration nodes for the IEC104 driver.
fn build_action_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "ioa".into(),
            label: ui_text!(en = "IOA", zh = "IOA"),
            data_type: UiDataType::Integer,
            default_value: None,
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "IOA is required", zh = "IOA 是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 65535.0,
                    message: Some(ui_text!(en = "Max 65535", zh = "最大为 65535")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "typeId".into(),
            label: ui_text!(en = "ASDU Type ID", zh = "ASDU类型"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(TypeID::C_SC_NA_1 as u8),
                        label: ui_text!(en = "Single Command No Time", zh = "单点命令无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::C_SC_TA_1 as u8),
                        label: ui_text!(
                            en = "Single Command With CP56Time2a Time",
                            zh = "单点命令带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_DC_NA_1 as u8),
                        label: ui_text!(en = "Double Command No Time", zh = "双点命令无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::C_DC_TA_1 as u8),
                        label: ui_text!(
                            en = "Double Command With CP56Time2a Time",
                            zh = "双点命令带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_RC_NA_1 as u8),
                        label: ui_text!(en = "Step Command No Time", zh = "步进命令无时标"),
                    },
                    EnumItem {
                        key: json!(TypeID::C_RC_TA_1 as u8),
                        label: ui_text!(
                            en = "Step Command With CP56Time2a Time",
                            zh = "步进命令带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_SE_NA_1 as u8),
                        label: ui_text!(
                            en = "Scaled Valuue Command No Time",
                            zh = "归一化值命令无时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_SE_TA_1 as u8),
                        label: ui_text!(
                            en = "Scaled Value Command With CP56Time2a Time",
                            zh = "归一化值命令带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_SE_NB_1 as u8),
                        label: ui_text!(
                            en = "Scaled Value Command No Time",
                            zh = "标度化值命令无时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_SE_TB_1 as u8),
                        label: ui_text!(
                            en = "Scaled Value Command With CP56Time2a Time",
                            zh = "标度化值命令带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_SE_NC_1 as u8),
                        label: ui_text!(
                            en = "Short Float Command No Time",
                            zh = "短浮点数命令无时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_SE_TC_1 as u8),
                        label: ui_text!(
                            en = "Short Float Command With CP56Time2a Time",
                            zh = "短浮点数命令带CP56Time2a时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_BO_NA_1 as u8),
                        label: ui_text!(
                            en = "32 Bit String Command No Time",
                            zh = "32比特串命令无时标"
                        ),
                    },
                    EnumItem {
                        key: json!(TypeID::C_BO_TA_1 as u8),
                        label: ui_text!(
                            en = "32 Bit String Command With CP56Time2a Time",
                            zh = "32比特串命令带CP56Time2a时标"
                        ),
                    },
                ],
            },
            default_value: None,
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Type ID is required",
                        zh = "类型 ID 是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}
