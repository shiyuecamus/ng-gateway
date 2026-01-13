use crate::{
    types::{DataBits, Dl645FunctionCode, Dl645Parity, StopBits},
    Dl645Version,
};
use ng_gateway_sdk::{
    ui_text, DriverSchemas, EnumItem, Field, Node, RuleValue, Rules, UiDataType, UiProps, Union,
    UnionCase,
};
use serde_json::json;

/// Build static metadata for DL/T 645 driver configuration.
///
/// This metadata describes how the UI should render forms for channel/device/
/// point/action configuration. The first version focuses on essential fields
/// only and can be expanded later without breaking binary compatibility.
pub(super) fn build_metadata() -> DriverSchemas {
    DriverSchemas {
        channel: build_channel_nodes(),
        device: build_device_nodes(),
        point: build_point_nodes(),
        action: build_action_nodes(),
    }
}

fn build_channel_nodes() -> Vec<Node> {
    vec![
        // Protocol version (common for all connection types)
        Node::Field(Box::new(Field {
            path: "version".into(),
            label: ui_text!(en = "Protocol Version", zh = "协议版本"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(Dl645Version::V1997 as i16),
                        label: ui_text!(en = "DL/T 645-1997", zh = "DL/T 645-1997"),
                    },
                    EnumItem {
                        key: json!(Dl645Version::V2007 as i16),
                        label: ui_text!(en = "DL/T 645-2007", zh = "DL/T 645-2007"),
                    },
                ],
            },
            default_value: Some(json!(Dl645Version::V2007 as i16)),
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Protocol version is required",
                        zh = "协议版本是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        // Connection kind selector
        Node::Field(Box::new(Field {
            path: "connection.kind".into(),
            label: ui_text!(en = "Connection Type", zh = "连接方式"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!("serial"),
                        label: ui_text!(en = "Serial", zh = "串口"),
                    },
                    EnumItem {
                        key: json!("tcp"),
                        label: ui_text!(en = "TCP", zh = "TCP"),
                    },
                ],
            },
            default_value: Some(json!("serial")),
            order: Some(4),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::Value(true)),
                ..Default::default()
            }),
            when: None,
        })),
        // Serial / TCP specific settings
        Node::Union(Union {
            order: Some(5),
            discriminator: "connection.kind".into(),
            mapping: vec![
                // Serial case
                UnionCase {
                    case_value: json!("serial"),
                    children: vec![
                        Node::Field(Box::new(Field {
                            path: "connection.port".into(),
                            label: ui_text!(en = "Serial Port", zh = "串口"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(1),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: Some(ui_text!(
                                        en = "Serial port is required",
                                        zh = "串口是必填项"
                                    )),
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.baudRate".into(),
                            label: ui_text!(en = "Baud Rate", zh = "波特率"),
                            data_type: UiDataType::Integer,
                            default_value: Some(json!(2400)),
                            order: Some(2),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::Value(true)),
                                min: Some(RuleValue::Value(300.0)),
                                max: Some(RuleValue::Value(4_000_000.0)),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.dataBits".into(),
                            label: ui_text!(en = "Data Bits", zh = "数据位"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!(DataBits::Five as i16),
                                        label: ui_text!(en = "5", zh = "5"),
                                    },
                                    EnumItem {
                                        key: json!(DataBits::Six as i16),
                                        label: ui_text!(en = "6", zh = "6"),
                                    },
                                    EnumItem {
                                        key: json!(DataBits::Seven as i16),
                                        label: ui_text!(en = "7", zh = "7"),
                                    },
                                    EnumItem {
                                        key: json!(DataBits::Eight as i16),
                                        label: ui_text!(en = "8", zh = "8"),
                                    },
                                ],
                            },
                            default_value: Some(json!(DataBits::Eight as i16)),
                            order: Some(3),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::Value(true)),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.stopBits".into(),
                            label: ui_text!(en = "Stop Bits", zh = "停止位"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!(StopBits::One as i16),
                                        label: ui_text!(en = "1", zh = "1"),
                                    },
                                    EnumItem {
                                        key: json!(StopBits::Two as i16),
                                        label: ui_text!(en = "2", zh = "2"),
                                    },
                                ],
                            },
                            default_value: Some(json!(StopBits::One as i16)),
                            order: Some(4),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::Value(true)),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.parity".into(),
                            label: ui_text!(en = "Parity", zh = "校验位"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!(Dl645Parity::None as i16),
                                        label: ui_text!(en = "None", zh = "无"),
                                    },
                                    EnumItem {
                                        key: json!(Dl645Parity::Odd as i16),
                                        label: ui_text!(en = "Odd", zh = "奇校验"),
                                    },
                                    EnumItem {
                                        key: json!(Dl645Parity::Even as i16),
                                        label: ui_text!(en = "Even", zh = "偶校验"),
                                    },
                                ],
                            },
                            default_value: Some(json!(Dl645Parity::Even as i16)),
                            order: Some(5),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::Value(true)),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                    ],
                },
                // TCP case
                UnionCase {
                    case_value: json!("tcp"),
                    children: vec![
                        Node::Field(Box::new(Field {
                            path: "connection.host".into(),
                            label: ui_text!(en = "TCP Host", zh = "TCP 主机"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(1),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: Some(ui_text!(
                                        en = "TCP host is required",
                                        zh = "TCP 主机是必填项"
                                    )),
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.port".into(),
                            label: ui_text!(en = "TCP Port", zh = "TCP 端口"),
                            data_type: UiDataType::Integer,
                            default_value: Some(json!(5001)),
                            order: Some(2),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::Value(true)),
                                min: Some(RuleValue::Value(1.0)),
                                max: Some(RuleValue::Value(65535.0)),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                    ],
                },
            ],
        }),
        // Advanced settings
        Node::Group(ng_gateway_sdk::Group {
            id: "advanced".into(),
            label: ui_text!(en = "Advanced Settings", zh = "高级设置"),
            description: None,
            collapsible: true,
            order: Some(6),
            children: vec![
                Node::Field(Box::new(Field {
                    path: "maxTimeouts".into(),
                    label: ui_text!(
                        en = "Max Timeouts",
                        zh = "最大超时数"
                    ),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(3)),
                    order: Some(4),
                    ui: Some(UiProps {
                        help: Some(ui_text!(
                            en = "Continuous timeouts exceeding this value will automatically trigger a reconnection",
                            zh = "连续超时超过此数值将会自动触发重连"
                        )),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::Value(0.0)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "wakeupPreamble".into(),
                    label: ui_text!(en = "Wakeup Preamble", zh = "前置唤醒码"),
                    data_type: UiDataType::Any,
                    default_value: Some(json!([0xFE, 0xFE, 0xFE, 0xFE])),
                    order: Some(5),
                    ui: None,
                    rules: None,
                    when: None,
                })),
            ],
        }),
    ]
}

fn build_device_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "address".into(),
            label: ui_text!(en = "Meter Address", zh = "电表地址"),
            data_type: UiDataType::String,
            default_value: None,
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Meter address is required",
                        zh = "电表地址是必填项"
                    )),
                }),
                pattern: Some(RuleValue::WithMessage {
                    value: "^[0-9]{12}$".to_string(),
                    message: Some(ui_text!(
                        en = "Meter address must be 12 digits",
                        zh = "电表地址必须为 12 位数字"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "password".into(),
            label: ui_text!(en = "Password", zh = "密码"),
            data_type: UiDataType::String,
            default_value: Some(json!("00000000")),
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Password is required", zh = "密码是必填项")),
                }),
                pattern: Some(RuleValue::WithMessage {
                    value: "^[0-9A-Fa-f]{1,16}$".to_string(),
                    message: Some(ui_text!(
                        en = "Password must be hex digits",
                        zh = "密码必须是十六进制数字"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "operatorCode".into(),
            label: ui_text!(en = "Operator Code", zh = "操作员代码"),
            data_type: UiDataType::String,
            default_value: Some(json!("00000000")),
            order: Some(3),
            ui: None,
            rules: Some(Rules {
                pattern: Some(RuleValue::WithMessage {
                    value: "^[0-9A-Fa-f]{1,16}$".to_string(),
                    message: Some(ui_text!(
                        en = "Operator code must be hex digits",
                        zh = "操作员代码必须是十六进制数字"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}

fn build_point_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "di".into(),
            label: ui_text!(en = "Data Identifier (DI)", zh = "数据标识 DI"),
            data_type: UiDataType::String,
            default_value: None,
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "DI is required", zh = "DI 是必填项")),
                }),
                pattern: Some(RuleValue::WithMessage {
                    value: "^[0-9A-Fa-f]{8}$".to_string(),
                    message: Some(ui_text!(
                        en = "DI must be 8 hex characters",
                        zh = "DI 必须为 8 位十六进制数"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "decimals".into(),
            label: ui_text!(en = "Decimals", zh = "小数位"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(2)),
            order: Some(2),
            ui: None,
            rules: None,
            when: None,
        })),
    ]
}

fn build_action_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "functionCode".into(),
            label: ui_text!(en = "Function Code", zh = "功能码"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(Dl645FunctionCode::WriteData as i16),
                        label: ui_text!(en = "Write Data", zh = "写数据"),
                    },
                    EnumItem {
                        key: json!(Dl645FunctionCode::BroadcastTimeSync as i16),
                        label: ui_text!(en = "Broadcast Time Sync", zh = "广播校时"),
                    },
                    EnumItem {
                        key: json!(Dl645FunctionCode::WriteAddress as i16),
                        label: ui_text!(en = "Write Address", zh = "写通信地址"),
                    },
                    EnumItem {
                        key: json!(Dl645FunctionCode::Freeze as i16),
                        label: ui_text!(en = "Freeze", zh = "冻结"),
                    },
                    EnumItem {
                        key: json!(Dl645FunctionCode::UpdateBaudRate as i16),
                        label: ui_text!(en = "Update Baud Rate", zh = "更改波特率"),
                    },
                    EnumItem {
                        key: json!(Dl645FunctionCode::ClearMaxDemand as i16),
                        label: ui_text!(en = "Clear Max Demand", zh = "最大需量清零"),
                    },
                    EnumItem {
                        key: json!(Dl645FunctionCode::ClearMeter as i16),
                        label: ui_text!(en = "Clear Meter", zh = "电表清零"),
                    },
                    EnumItem {
                        key: json!(Dl645FunctionCode::ClearEvents as i16),
                        label: ui_text!(en = "Clear Events", zh = "事件清零"),
                    },
                ],
            },
            default_value: Some(json!(Dl645FunctionCode::ReadData as i16)),
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::Value(true)),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "di".into(),
            label: ui_text!(en = "Data Identifier (DI)", zh = "数据标识 DI"),
            data_type: UiDataType::String,
            default_value: None,
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                pattern: Some(RuleValue::WithMessage {
                    value: "^[0-9A-Fa-f]{8}$".to_string(),
                    message: Some(ui_text!(
                        en = "DI must be 8 hex characters",
                        zh = "DI 必须为 8 位十六进制数"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "decimals".into(),
            label: ui_text!(en = "Decimals", zh = "小数位"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(3)),
            order: Some(3),
            ui: None,
            rules: None,
            when: None,
        })),
    ]
}
