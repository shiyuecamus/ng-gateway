use crate::types::{SerialDataBits, SerialStopBits};
use ng_gateway_sdk::{
    ui_text, DriverSchemas, EnumItem, Field, Group, Node, RuleValue, Rules, UiDataType, UiProps,
    Union, UnionCase,
};
use serde_json::json;

/// Build static metadata for CJ/T 188 driver configuration.
pub(super) fn build_metadata() -> DriverSchemas {
    DriverSchemas {
        channel: build_channel_nodes(),
        device: build_device_nodes(),
        point: build_point_nodes(),
        // CJ/T 188 southward driver is read-only for now: downlink (actions / write_point)
        // is explicitly not supported.
        action: Vec::new(),
    }
}

fn build_channel_nodes() -> Vec<Node> {
    vec![
        // Protocol version
        Node::Field(Box::new(Field {
            path: "version".into(),
            label: ui_text!(en = "Protocol Version", zh = "协议版本"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!("V2004"),
                        label: ui_text!(en = "CJ/T 188-2004", zh = "CJ/T 188-2004"),
                    },
                    EnumItem {
                        key: json!("V2018"),
                        label: ui_text!(en = "CJ/T 188-2018", zh = "CJ/T 188-2018"),
                    },
                ],
            },
            default_value: Some(json!("V2004")),
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
            path: "connection.type".into(),
            label: ui_text!(en = "Connection Type", zh = "连接方式"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!("Serial"),
                        label: ui_text!(en = "Serial", zh = "串口"),
                    },
                    EnumItem {
                        key: json!("Tcp"),
                        label: ui_text!(en = "TCP", zh = "TCP"),
                    },
                ],
            },
            default_value: Some(json!("Serial")),
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
            discriminator: "connection.type".into(),
            mapping: vec![
                // Serial case
                UnionCase {
                    case_value: json!("Serial"),
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
                            path: "connection.baud_rate".into(),
                            label: ui_text!(en = "Baud Rate", zh = "波特率"),
                            data_type: UiDataType::Integer,
                            default_value: Some(json!(9600)),
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
                            path: "connection.data_bits".into(),
                            label: ui_text!(en = "Data Bits", zh = "数据位"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!(SerialDataBits::Five as i16),
                                        label: ui_text!(en = "5", zh = "5"),
                                    },
                                    EnumItem {
                                        key: json!(SerialDataBits::Six as i16),
                                        label: ui_text!(en = "6", zh = "6"),
                                    },
                                    EnumItem {
                                        key: json!(SerialDataBits::Seven as i16),
                                        label: ui_text!(en = "7", zh = "7"),
                                    },
                                    EnumItem {
                                        key: json!(SerialDataBits::Eight as i16),
                                        label: ui_text!(en = "8", zh = "8"),
                                    },
                                ],
                            },
                            default_value: Some(json!(SerialDataBits::Eight as i16)),
                            order: Some(3),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::Value(true)),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.stop_bits".into(),
                            label: ui_text!(en = "Stop Bits", zh = "停止位"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!(SerialStopBits::One as i16),
                                        label: ui_text!(en = "1", zh = "1"),
                                    },
                                    EnumItem {
                                        key: json!(SerialStopBits::Two as i16),
                                        label: ui_text!(en = "2", zh = "2"),
                                    },
                                ],
                            },
                            default_value: Some(json!(SerialStopBits::One as i16)),
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
                                        key: json!("None"),
                                        label: ui_text!(en = "None", zh = "无"),
                                    },
                                    EnumItem {
                                        key: json!("Odd"),
                                        label: ui_text!(en = "Odd", zh = "奇校验"),
                                    },
                                    EnumItem {
                                        key: json!("Even"),
                                        label: ui_text!(en = "Even", zh = "偶校验"),
                                    },
                                ],
                            },
                            default_value: Some(json!("Even")),
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
                    case_value: json!("Tcp"),
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
        Node::Group(Group {
            id: "advanced".into(),
            label: ui_text!(en = "Advanced Settings", zh = "高级设置"),
            description: None,
            collapsible: true,
            order: Some(6),
            children: vec![
                Node::Field(Box::new(Field {
                    path: "max_timeouts".into(),
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
                    path: "wakeup_preamble".into(),
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
    // Enumerate meter types 0x10..=0x49 as requested.
    // We keep labels explicit for common codes and generic for the rest.
    let mut meter_type_items: Vec<EnumItem> = Vec::with_capacity(0x49 - 0x10 + 1);
    for code in 0x10u8..=0x49u8 {
        let label_en = match code {
            0x10 => "0x10 (Cold Water)",
            0x11 => "0x11 (Domestic Hot Water)",
            0x12 => "0x12 (Drinking Water)",
            0x13 => "0x13 (Reclaimed Water)",
            0x20 => "0x20 (Heat)",
            0x21 => "0x21 (Cooling)",
            0x22 => "0x22 (Heat + Cooling)",
            0x30 => "0x30 (Gas)",
            0x40 => "0x40 (Custom)",
            _ if (0x10..=0x19).contains(&code) => "0x?? (Water family)",
            _ if (0x20..=0x29).contains(&code) => "0x?? (Heat family)",
            _ if (0x30..=0x39).contains(&code) => "0x?? (Gas family)",
            _ => "0x?? (Custom family)",
        };

        let label_zh = match code {
            0x10 => "0x10（冷水水表）",
            0x11 => "0x11（生活热水水表）",
            0x12 => "0x12（直饮水水表）",
            0x13 => "0x13（中水水表）",
            0x20 => "0x20（热量表-计热量）",
            0x21 => "0x21（热量表-计冷量）",
            0x22 => "0x22（热量表-计热量+冷量）",
            0x30 => "0x30（燃气表）",
            0x40 => "0x40（自定义表）",
            _ if (0x10..=0x19).contains(&code) => "0x??（水表族）",
            _ if (0x20..=0x29).contains(&code) => "0x??（热量表族）",
            _ if (0x30..=0x39).contains(&code) => "0x??（燃气表族）",
            _ => "0x??（自定义表族）",
        };

        let label_en = label_en.replace("0x??", &format!("0x{code:02X}"));
        let label_zh = label_zh.replace("0x??", &format!("0x{code:02X}"));

        meter_type_items.push(EnumItem {
            key: json!(code as i16),
            label: ui_text!(en = label_en, zh = label_zh),
        });
    }

    vec![
        Node::Field(Box::new(Field {
            path: "meterType".into(),
            label: ui_text!(en = "Meter Type (T)", zh = "表类型(T)"),
            data_type: UiDataType::Enum {
                items: meter_type_items,
            },
            default_value: Some(json!(0x10i16)),
            order: Some(1),
            ui: Some(UiProps {
                help: Some(ui_text!(
                    en = "CJ/T 188-2018 meter type code (T).",
                    zh = "CJ/T 188-2018 表类型码(T)。"
                )),
                ..Default::default()
            }),
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Meter type is required",
                        zh = "表类型(T)是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "address".into(),
            label: ui_text!(en = "Meter Address (A6..A0)", zh = "表具地址(A6..A0)"),
            data_type: UiDataType::String,
            default_value: None,
            order: Some(2),
            ui: Some(UiProps {
                help: Some(ui_text!(
                    en = "Format: 14 hex characters representing A6..A0 bytes (high address first). The protocol wire order is A0..A6 (low byte first).",
                    zh = "格式：14位hex字符串，表示 A6..A0 字节（高位在前）。协议传输次序为 A0..A6（低位字节先传）。"
                )),
                ..Default::default()
            }),
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Meter address is required",
                        zh = "表具地址是必填项"
                    )),
                }),
                pattern: Some(RuleValue::WithMessage {
                    value: "^[0-9A-Fa-f]{14}$".to_string(),
                    message: Some(ui_text!(
                        en = "Address must be 14 hex characters",
                        zh = "地址必须为 14 位十六进制字符"
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
            ui: Some(UiProps {
                help: Some(ui_text!(
                    en = "2-byte Data Identifier in Hex (e.g. 901F)",
                    zh = "2字节数据标识符，十六进制 (例如 901F)"
                )),
                ..Default::default()
            }),
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "DI is required", zh = "DI 是必填项")),
                }),
                pattern: Some(RuleValue::WithMessage {
                    value: "^[0-9A-Fa-f]{4}$".to_string(),
                    message: Some(ui_text!(
                        en = "DI must be 4 hex characters",
                        zh = "DI 必须为 4 位十六进制数"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "field_key".into(),
            label: ui_text!(en = "Field Key", zh = "字段键 field_key"),
            data_type: UiDataType::String,
            default_value: None,
            order: Some(2),
            ui: Some(UiProps {
                help: Some(ui_text!(
                    en = "Field key inside this DI response schema (e.g. current_flow, datetime). See docs for the full list.",
                    zh = "该 DI 响应结构中的字段键（例如 current_flow、datetime）。完整列表见文档。"
                )),
                ..Default::default()
            }),
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "field_key is required",
                        zh = "field_key 是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}
