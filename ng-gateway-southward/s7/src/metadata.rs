use ng_gateway_sdk::{
    ui_text, DriverSchemas, EnumItem, Field, Node, RuleValue, Rules, UiDataType, Union, UnionCase,
};
use serde_json::json;

use crate::protocol::frame::CpuType;

/// Build static metadata once to be embedded as JSON for the gateway UI/config
pub(super) fn build_metadata() -> DriverSchemas {
    DriverSchemas {
        channel: build_channel_nodes(),
        device: build_device_nodes(),
        point: build_point_nodes(),
        action: build_action_nodes(),
    }
}

/// Build channel-level configuration nodes for the S7 driver.
fn build_channel_nodes() -> Vec<Node> {
    vec![
            Node::Field(Box::new(Field {
                path: "cpu".into(),
                label: ui_text!(en = "CPU Type", zh = "CPU 类型"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!(CpuType::S7200 as u16),
                            label: ui_text!(en = "S7200", zh = "S7200")
                        },
                        EnumItem {
                            key: json!(CpuType::S7200Smart as u16),
                            label: ui_text!(en = "S7200Smart", zh = "S7200Smart")
                        },
                        EnumItem {
                            key: json!(CpuType::S7300 as u16),
                            label: ui_text!(en = "S7300", zh = "S7300")
                        },
                        EnumItem {
                            key: json!(CpuType::S7400 as u16),
                            label: ui_text!(en = "S7400", zh = "S7400")
                        },
                        EnumItem {
                            key: json!(CpuType::S71200 as u16),
                            label: ui_text!(en = "S1200", zh = "S1200")
                        },
                        EnumItem {
                            key: json!(CpuType::S71500 as u16),
                            label: ui_text!(en = "S1500", zh = "S1500")
                        },
                        EnumItem {
                            key: json!(CpuType::Logo0BA8 as u16),
                            label: ui_text!(en = "Logo0BA8", zh = "Logo0BA8")
                        },
                    ],
                },
                default_value: Some(json!(CpuType::S71500 as u16)),
                order: Some(1),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(en = "CPU Type is required", zh = "CPU 类型是必填项"))
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "host".into(),
                label: ui_text!(en = "Host", zh = "主机"),
                data_type: UiDataType::String,
                default_value: None,
                order: Some(2),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(en = "Host is required", zh = "主机是必填项"))
                    }),
                    pattern: Some(RuleValue::WithMessage {
                        value: "^(?:(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)(?:\\.(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?))*|(?:(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)\\.){3}(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d))$".to_string(),
                        message: Some(ui_text!(en = "Enter a valid IPv4 address or hostname (no schema/port)", zh = "请输入有效的 IPv4 或主机名（不含协议和端口）")),
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "port".into(),
                label: ui_text!(en = "Port", zh = "端口"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(102)),
                order: Some(3),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(en = "Port is required", zh = "端口是必填项"))
                    }),
                    min: Some(RuleValue::WithMessage {
                        value: 1.0,
                        message: Some(ui_text!(en = "Port must be > 0", zh = "端口必须大于0"))
                    }),
                    max: Some(RuleValue::WithMessage {
                        value: 65535.0,
                        message: Some(ui_text!(en = "Port must be < 65536", zh = "端口必须小于65536"))
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            // TSAP selector
            Node::Field(Box::new(Field {
                path: "tsap.kind".into(),
                label: ui_text!(en = "TSAP Mode", zh = "TSAP 模式"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!("rackSlot"),
                            label: ui_text!(en = "Rack/Slot", zh = "机架/插槽")
                        },
                        EnumItem {
                            key: json!("tsap"),
                            label: ui_text!(en = "Explicit TSAP", zh = "显式 TSAP")
                        },
                    ],
                },
                default_value: Some(json!("rackSlot")),
                order: Some(4),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: None
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Union(Union {
                order: Some(5),
                discriminator: "tsap.kind".into(),
                mapping: vec![
                    UnionCase {
                        case_value: json!("rackSlot"),
                        children: vec![
                            Node::Field(Box::new(Field {
                                path: "tsap.rack".into(),
                                label: ui_text!(en = "Rack", zh = "机架"),
                                data_type: UiDataType::Integer,
                                default_value: Some(json!(0)),
                                order: Some(1),
                                ui: None,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(en = "Rack is required", zh = "机架是必填项"))
                                    }),
                                    min: Some(RuleValue::WithMessage {
                                        value: 0.0,
                                        message: None
                                    }),
                                     max: Some(RuleValue::WithMessage {
                                        value: 7.0,
                                        message: None
                                    }),
                                    ..Default::default()
                                }),
                                when: None,
                            })),
                            Node::Field(Box::new(Field {
                                path: "tsap.slot".into(),
                                label: ui_text!(en = "Slot", zh = "插槽"),
                                data_type: UiDataType::Integer,
                                default_value: Some(json!(1)),
                                order: Some(2),
                                ui: None,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(en = "Slot is required", zh = "插槽是必填项"))
                                    }),
                                    min: Some(RuleValue::WithMessage {
                                        value: 0.0,
                                        message: None
                                    }),
                                    max: Some(RuleValue::WithMessage {
                                        value: 31.0,
                                        message: None
                                    }),
                                    ..Default::default()
                                }),
                                when: None,
                            })),
                        ],
                    },
                    UnionCase {
                        case_value: json!("tsap"),
                        children: vec![
                            Node::Field(Box::new(Field {
                                path: "tsap.src".into(),
                                label: ui_text!(en = "TSAP SRC", zh = "TSAP 源"),
                                data_type: UiDataType::Integer,
                                default_value: None,
                                order: Some(1),
                                ui: None,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(en = "TSAP SRC is required", zh = "TSAP 源是必填项"))
                                    }),
                                    min: Some(RuleValue::WithMessage {
                                        value: 0.0,
                                        message: None
                                    }),
                                    max: Some(RuleValue::Value(65535.0)),
                                    ..Default::default()
                                }),
                                when: None,
                            })),
                            Node::Field(Box::new(Field {
                                path: "tsap.dst".into(),
                                label: ui_text!(en = "TSAP DST", zh = "TSAP 目标"),
                                data_type: UiDataType::Integer,
                                default_value: None,
                                order: Some(2),
                                ui: None,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(en = "TSAP DST is required", zh = "TSAP 目标是必填项"))
                                    }),
                                    min: Some(RuleValue::WithMessage {
                                        value: 0.0,
                                        message: None
                                    }),
                                    max: Some(RuleValue::WithMessage {
                                        value: 65535.0,
                                        message: None
                                    }),
                                    ..Default::default()
                                }),
                                when: None,
                            })),
                        ],
                    },
                ],
            }),
            // Preferred negotiation values
            Node::Field(Box::new(Field {
                path: "preferredPduSize".into(),
                label: ui_text!(en = "Preferred PDU Size", zh = "PDU 大小"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(960)),
                order: Some(6),
                ui: None,
                rules: Some(Rules {
                    min: Some(RuleValue::Value(128.0)),
                    max: Some(RuleValue::Value(8192.0)),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "preferredAmqCaller".into(),
                label: ui_text!(en = "Preferred AMQ Caller", zh = "AMQ Caller"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(8)),
                order: Some(7),
                ui: None,
                rules: Some(Rules {
                    min: Some(RuleValue::Value(1.0)),
                    max: Some(RuleValue::Value(255.0)),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "preferredAmqCallee".into(),
                label: ui_text!(en = "Preferred AMQ Callee", zh = "AMQ Callee"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(80)),
                order: Some(8),
                ui: None,
                rules: Some(Rules {
                    min: Some(RuleValue::Value(1.0)),
                    max: Some(RuleValue::Value(255.0)),
                    ..Default::default()
                }),
                when: None,
            })),
        ]
}

/// Build device-level configuration nodes for the S7 driver.
fn build_device_nodes() -> Vec<Node> {
    vec![]
}

/// Build point-level configuration nodes for the S7 driver.
fn build_point_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "address".into(),
        label: ui_text!(en = "S7 Address", zh = "S7 地址"),
        data_type: UiDataType::String,
        default_value: None,
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: Some(ui_text!(en = "Address is required", zh = "地址是必填项")),
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}

/// Build action-level configuration nodes for the S7 driver.
fn build_action_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "address".into(),
        label: ui_text!(en = "S7 Address", zh = "S7 地址"),
        data_type: UiDataType::String,
        default_value: None,
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: Some(ui_text!(en = "Address is required", zh = "地址是必填项")),
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}
