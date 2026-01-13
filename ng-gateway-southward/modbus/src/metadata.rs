use ng_gateway_sdk::{
    ui_text, DriverSchemas, EnumItem, Field, Node, RuleValue, Rules, UiDataType, Union, UnionCase,
};
use serde_json::json;

use crate::types::{DataBits, Endianness, ModbusFunctionCode, Parity, StopBits};

/// Build static metadata once to be embedded as JSON for the gateway UI/config.
pub(super) fn build_metadata() -> DriverSchemas {
    DriverSchemas {
        channel: build_channel_nodes(),
        device: build_device_nodes(),
        point: build_point_nodes(),
        action: build_action_nodes(),
    }
}

/// Build channel-level configuration nodes for the Modbus driver.
fn build_channel_nodes() -> Vec<Node> {
    vec![
            Node::Field(Box::new(Field {
                path: "connection.kind".into(), // write a parallel kind for UI discrimination
                label: ui_text!(en = "Connection Type", zh = "连接方式"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!("tcp"),
                            label: ui_text!(en = "TCP", zh = "TCP"),
                        },
                        EnumItem {
                            key: json!("rtu"),
                            label: ui_text!(en = "RTU", zh = "RTU"),
                        },
                    ],
                },
                default_value: Some(json!("tcp")),
                order: Some(1),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(
                            en = "Connection Type is required",
                            zh = "连接方式是必填项"
                        )),
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Union(Union {
                order: Some(2),
                discriminator: "connection.kind".into(),
                mapping: vec![
                    UnionCase {
                        case_value: json!("tcp"),
                        children: vec![
                            Node::Field(Box::new(Field {
                                path: "connection.host".into(),
                                label: ui_text!(en = "Host", zh = "主机"),
                                data_type: UiDataType::String,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(
                                            en = "Host is required",
                                            zh = "主机是必填项"
                                        )),
                                    }),
                                    // Hostname (RFC-1123 labels, no leading/trailing hyphen) OR IPv4
                                    pattern: Some(RuleValue::WithMessage {
                                        value: "^(?:(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)(?:\\.(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?))*|(?:(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)\\.){3}(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d))$".to_string(),
                                        message: Some(ui_text!(
                                            en = "Enter a valid IPv4 address or hostname (no schema/port)",
                                            zh = "请输入有效的 IPv4 或主机名（不含协议和端口）"
                                        )),
                                    }),
                                    ..Default::default()
                                }),
                                default_value: None,
                                order: Some(1),
                                ui: None,
                                when: None,
                            })),
                            Node::Field(Box::new(Field {
                                path: "connection.port".into(),
                                label: ui_text!(en = "Port", zh = "端口"),
                                data_type: UiDataType::Integer,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(
                                            en = "Port is required",
                                            zh = "端口是必填项"
                                        )),
                                    }),
                                    min: Some(RuleValue::WithMessage {
                                        value: 1.0,
                                        message: Some(ui_text!(
                                            en = "Port must be greater than 0",
                                            zh = "端口必须大于0"
                                        )),
                                    }),
                                    max: Some(RuleValue::WithMessage {
                                        value: 65535.0,
                                        message: Some(ui_text!(
                                            en = "Port must be less than 65536",
                                            zh = "端口必须小于65536"
                                        )),
                                    }),
                                    ..Default::default()
                                }),
                                default_value: Some(json!(502)),
                                order: Some(1),
                                ui: None,
                                when: None,
                            })),
                        ],
                    },
                    UnionCase {
                        case_value: json!("rtu"),
                        children: vec![
                            Node::Field(Box::new(Field {
                                path: "connection.port".into(),
                                label: ui_text!(en = "Serial Port", zh = "串口"),
                                data_type: UiDataType::String,
                                default_value: None,
                                order: Some(2),
                                ui: None,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(
                                            en = "Serial Port is required",
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
                                default_value: Some(json!(9600)),
                                order: Some(3),
                                ui: None,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(
                                            en = "Baud Rate is required",
                                            zh = "波特率是必填项"
                                        )),
                                    }),
                                    min: Some(RuleValue::WithMessage {
                                        value: 300.0,
                                        message: Some(ui_text!(
                                            en = "Baud Rate must be greater than 300",
                                            zh = "波特率必须大于300"
                                        )),
                                    }),
                                    max: Some(RuleValue::WithMessage {
                                        value: 4_000_000.0,
                                        message: Some(ui_text!(
                                            en = "Baud Rate must be less than 4000000",
                                            zh = "波特率必须小于4000000"
                                        )),
                                    }),
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
                                            key: json!(DataBits::Five as u8),
                                            label: ui_text!(en = "5", zh = "5"),
                                        },
                                        EnumItem {
                                            key: json!(DataBits::Six as u8),
                                            label: ui_text!(en = "6", zh = "6"),
                                        },
                                        EnumItem {
                                            key: json!(DataBits::Seven as u8),
                                            label: ui_text!(en = "7", zh = "7"),
                                        },
                                        EnumItem {
                                            key: json!(DataBits::Eight as u8),
                                            label: ui_text!(en = "8", zh = "8"),
                                        },
                                    ],
                                },
                                default_value: Some(json!(DataBits::Eight as u8)),
                                order: Some(4),
                                ui: None,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(
                                            en = "Data Bits is required",
                                            zh = "数据位是必填项"
                                        )),
                                    }),
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
                                            key: json!(StopBits::One as u8),
                                            label: ui_text!(en = "1", zh = "1"),
                                        },
                                        EnumItem {
                                            key: json!(StopBits::Two as u8),
                                            label: ui_text!(en = "2", zh = "2"),
                                        },
                                    ],
                                },
                                default_value: Some(json!(StopBits::One as u8)),
                                order: Some(5),
                                ui: None,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(
                                            en = "Stop Bits is required",
                                            zh = "停止位是必填项"
                                        )),
                                    }),
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
                                            key: json!(Parity::None as u8),
                                            label: ui_text!(en = "None", zh = "无"),
                                        },
                                        EnumItem {
                                            key: json!(Parity::Odd as u8),
                                            label: ui_text!(en = "Odd", zh = "奇校验"),
                                        },
                                        EnumItem {
                                            key: json!(Parity::Even as u8),
                                            label: ui_text!(en = "Even", zh = "偶校验"),
                                        },
                                    ],
                                },
                                default_value: Some(json!(Parity::None as u8)),
                                order: Some(6),
                                ui: None,
                                rules: Some(Rules {
                                    required: Some(RuleValue::WithMessage {
                                        value: true,
                                        message: Some(ui_text!(
                                            en = "Parity is required",
                                            zh = "校验位是必填项"
                                        )),
                                    }),
                                    ..Default::default()
                                }),
                                when: None,
                            })),
                        ],
                    },
                ],
            }),
            Node::Field(Box::new(Field {
                path: "byteOrder".into(),
                label: ui_text!(en = "Byte Order", zh = "字节序"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!(Endianness::BigEndian as u8),
                            label: ui_text!(en = "Big Endian", zh = "大端序"),
                        },
                        EnumItem {
                            key: json!(Endianness::LittleEndian as u8),
                            label: ui_text!(en = "Little Endian", zh = "小端序"),
                        },
                    ],
                },
                default_value: Some(json!(Endianness::BigEndian as u8)),
                order: Some(6),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(
                            en = "Byte Order is required",
                            zh = "字节序是必填项"
                        )),
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "wordOrder".into(),
                label: ui_text!(en = "Word Order", zh = "字节序"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!(Endianness::BigEndian as u8),
                            label: ui_text!(en = "Big Endian", zh = "大端序"),
                        },
                        EnumItem {
                            key: json!(Endianness::LittleEndian as u8),
                            label: ui_text!(en = "Little Endian", zh = "小端序"),
                        },
                    ],
                },
                default_value: Some(json!(Endianness::BigEndian as u8)),
                order: Some(7),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(
                            en = "Word Order is required",
                            zh = "字节序是必填项"
                        )),
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "maxGap".into(),
                label: ui_text!(en = "Max Gap", zh = "最大间隙"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(10)),
                order: Some(8),
                ui: None,
                rules: Some(Rules {
                    min: Some(RuleValue::WithMessage {
                        value: 0.0,
                        message: Some(ui_text!(
                            en = "Max Gap must be greater than 0",
                            zh = "最大间隙必须大于0"
                        )),
                    }),
                    max: Some(RuleValue::WithMessage {
                        value: 2000.0,
                        message: Some(ui_text!(
                            en = "Max Gap must be less than 2000",
                            zh = "最大间隙必须小于2000"
                        )),
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "maxBatch".into(),
                label: ui_text!(en = "Max Batch", zh = "最大批量"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(100)),
                order: Some(9),
                ui: None,
                rules: Some(Rules {
                    min: Some(RuleValue::WithMessage {
                        value: 1.0,
                        message: Some(ui_text!(
                            en = "Max Batch must be greater than 1",
                            zh = "最大批量必须大于1"
                        )),
                    }),
                    max: Some(RuleValue::WithMessage {
                        value: 125.0,
                        message: Some(ui_text!(
                            en = "Max Batch must be less than 125",
                            zh = "最大批量必须小于125"
                        )),
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
        ]
}

/// Build device-level configuration nodes for the Modbus driver.
fn build_device_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "slaveId".into(),
        label: ui_text!(en = "Slave ID", zh = "从站ID"),
        data_type: UiDataType::Integer,
        default_value: None,
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: Some(ui_text!(en = "Slave ID is required", zh = "从站ID是必填项")),
            }),
            min: Some(RuleValue::WithMessage {
                value: 1.0,
                message: Some(ui_text!(
                    en = "Slave ID must be greater than 1",
                    zh = "从站ID必须大于1"
                )),
            }),
            max: Some(RuleValue::WithMessage {
                value: 247.0,
                message: Some(ui_text!(
                    en = "Slave ID must be less than 247",
                    zh = "从站ID必须小于247"
                )),
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}

/// Build point-level configuration nodes for the Modbus driver.
fn build_point_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "functionCode".into(),
            label: ui_text!(en = "Function Code", zh = "功能码"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(ModbusFunctionCode::ReadCoils as u8),
                        label: ui_text!(en = "Read Coils", zh = "读线圈"),
                    },
                    EnumItem {
                        key: json!(ModbusFunctionCode::ReadDiscreteInputs as u8),
                        label: ui_text!(en = "Read Discrete Inputs", zh = "读离散输入"),
                    },
                    EnumItem {
                        key: json!(ModbusFunctionCode::ReadHoldingRegisters as u8),
                        label: ui_text!(en = "Read Holding Registers", zh = "读保持寄存器"),
                    },
                    EnumItem {
                        key: json!(ModbusFunctionCode::ReadInputRegisters as u8),
                        label: ui_text!(en = "Read Input Registers", zh = "读输入寄存器"),
                    },
                ],
            },
            default_value: None,
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Function Code is required",
                        zh = "功能码是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "address".into(),
            label: ui_text!(en = "Address", zh = "地址"),
            data_type: UiDataType::Integer,
            default_value: None,
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Address is required", zh = "地址是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(
                        en = "Address must be greater than 0",
                        zh = "地址必须大于0"
                    )),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 65535.0,
                    message: Some(ui_text!(
                        en = "Address must be less than 65536",
                        zh = "地址必须小于65536"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "quantity".into(),
            label: ui_text!(en = "Quantity", zh = "数量"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(1)),
            order: Some(3),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Quantity is required", zh = "数量是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: Some(ui_text!(
                        en = "Quantity must be greater than 1",
                        zh = "数量必须大于1"
                    )),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 125.0,
                    message: Some(ui_text!(
                        en = "Quantity must be less than 125",
                        zh = "数量必须小于125"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}

/// Build action-level configuration nodes for the Modbus driver.
fn build_action_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "functionCode".into(),
            label: ui_text!(en = "Function Code", zh = "功能码"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(ModbusFunctionCode::WriteSingleCoil as u8),
                        label: ui_text!(en = "Write Single Coil", zh = "写单个线圈"),
                    },
                    EnumItem {
                        key: json!(ModbusFunctionCode::WriteSingleRegister as u8),
                        label: ui_text!(en = "Write Single Register", zh = "写单个寄存器"),
                    },
                    EnumItem {
                        key: json!(ModbusFunctionCode::WriteMultipleCoils as u8),
                        label: ui_text!(en = "Write Multiple Coils", zh = "写多个线圈"),
                    },
                    EnumItem {
                        key: json!(ModbusFunctionCode::WriteMultipleRegisters as u8),
                        label: ui_text!(en = "Write Multiple Registers", zh = "写多个寄存器"),
                    },
                ],
            },
            default_value: None,
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Function Code is required",
                        zh = "功能码是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "address".into(),
            label: ui_text!(en = "Address", zh = "地址"),
            data_type: UiDataType::Integer,
            default_value: None,
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Address is required", zh = "地址是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(
                        en = "Address must be greater than 0",
                        zh = "地址必须大于0"
                    )),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 65535.0,
                    message: Some(ui_text!(
                        en = "Address must be less than 65536",
                        zh = "地址必须小于65536"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "quantity".into(),
            label: ui_text!(en = "Quantity", zh = "数量"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(1)),
            order: Some(3),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Quantity is required", zh = "数量是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: Some(ui_text!(
                        en = "Quantity must be greater than 1",
                        zh = "数量必须大于1"
                    )),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 125.0,
                    message: Some(ui_text!(
                        en = "Quantity must be less than 125",
                        zh = "数量必须小于125"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}
