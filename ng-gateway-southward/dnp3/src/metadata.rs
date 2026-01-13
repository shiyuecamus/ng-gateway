use crate::types::{Dnp3CommandType, Dnp3PointGroup};
use ng_gateway_sdk::{
    ui_text, DriverSchemas, EnumItem, Field, Group, Node, Operator, RuleValue, Rules, UiDataType,
    UiProps, Union, UnionCase, When, WhenEffect,
};
use serde_json::json;

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
        // Connection Selector
        Node::Field(Box::new(Field {
            path: "connection.type".into(),
            label: ui_text!(en = "Connection Type", zh = "连接类型"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!("tcp"),
                        label: ui_text!(en = "TCP", zh = "TCP"),
                    },
                    EnumItem {
                        key: json!("udp"),
                        label: ui_text!(en = "UDP", zh = "UDP"),
                    },
                    EnumItem {
                        key: json!("serial"),
                        label: ui_text!(en = "Serial", zh = "串口"),
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
                        en = "Connection type is required",
                        zh = "连接类型是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        // Connection Specifics
        Node::Union(Union {
            order: Some(2),
            discriminator: "connection.type".into(),
            mapping: vec![
                // TCP
                UnionCase {
                    case_value: json!("tcp"),
                    children: vec![
                        Node::Field(Box::new(Field {
                            path: "connection.host".into(),
                            label: ui_text!(en = "Host", zh = "主机"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(1),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: Some(ui_text!(
                                        en = "Host is required",
                                        zh = "主机是必填项"
                                    )),
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.port".into(),
                            label: ui_text!(en = "Port", zh = "端口"),
                            data_type: UiDataType::Integer,
                            default_value: Some(json!(20000)),
                            order: Some(2),
                            ui: None,
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
                                        en = "Port must be > 0",
                                        zh = "端口必须大于0"
                                    )),
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
                    ],
                },
                // UDP
                UnionCase {
                    case_value: json!("udp"),
                    children: vec![
                        Node::Field(Box::new(Field {
                            path: "connection.host".into(),
                            label: ui_text!(en = "Remote Host", zh = "远程主机"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(1),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: Some(ui_text!(
                                        en = "Host is required",
                                        zh = "主机是必填项"
                                    )),
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.port".into(),
                            label: ui_text!(en = "Remote Port", zh = "远程端口"),
                            data_type: UiDataType::Integer,
                            default_value: Some(json!(20000)),
                            order: Some(2),
                            ui: None,
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
                                        en = "Port must be > 0",
                                        zh = "端口必须大于0"
                                    )),
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
                        Node::Field(Box::new(Field {
                            path: "connection.localPort".into(),
                            label: ui_text!(en = "Local Port", zh = "本地端口"),
                            data_type: UiDataType::Integer,
                            default_value: None,
                            order: Some(3),
                            ui: None,
                            rules: Some(Rules {
                                min: Some(RuleValue::WithMessage {
                                    value: 1.0,
                                    message: Some(ui_text!(
                                        en = "Port must be > 0",
                                        zh = "端口必须大于0"
                                    )),
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
                    ],
                },
                // Serial
                UnionCase {
                    case_value: json!("serial"),
                    children: vec![
                        Node::Field(Box::new(Field {
                            path: "connection.path".into(),
                            label: ui_text!(en = "Serial Path", zh = "串口路径"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(1),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: Some(ui_text!(
                                        en = "Path is required",
                                        zh = "串口路径是必填项"
                                    )),
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.baudRate".into(),
                            label: ui_text!(en = "Baud Rate", zh = "波特率"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!(9600),
                                        label: ui_text!(en = "9600", zh = "9600"),
                                    },
                                    EnumItem {
                                        key: json!(19200),
                                        label: ui_text!(en = "19200", zh = "19200"),
                                    },
                                    EnumItem {
                                        key: json!(38400),
                                        label: ui_text!(en = "38400", zh = "38400"),
                                    },
                                    EnumItem {
                                        key: json!(57600),
                                        label: ui_text!(en = "57600", zh = "57600"),
                                    },
                                    EnumItem {
                                        key: json!(115200),
                                        label: ui_text!(en = "115200", zh = "115200"),
                                    },
                                ],
                            },
                            default_value: Some(json!(9600)),
                            order: Some(2),
                            ui: None,
                            rules: None,
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.dataBits".into(),
                            label: ui_text!(en = "Data Bits", zh = "数据位"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!(5),
                                        label: ui_text!(en = "5", zh = "5"),
                                    },
                                    EnumItem {
                                        key: json!(6),
                                        label: ui_text!(en = "6", zh = "6"),
                                    },
                                    EnumItem {
                                        key: json!(7),
                                        label: ui_text!(en = "7", zh = "7"),
                                    },
                                    EnumItem {
                                        key: json!(8),
                                        label: ui_text!(en = "8", zh = "8"),
                                    },
                                ],
                            },
                            default_value: Some(json!(8)),
                            order: Some(3),
                            ui: None,
                            rules: None,
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.stopBits".into(),
                            label: ui_text!(en = "Stop Bits", zh = "停止位"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!(1),
                                        label: ui_text!(en = "1", zh = "1"),
                                    },
                                    EnumItem {
                                        key: json!(2),
                                        label: ui_text!(en = "2", zh = "2"),
                                    },
                                ],
                            },
                            default_value: Some(json!(1)),
                            order: Some(4),
                            ui: None,
                            rules: None,
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "connection.parity".into(),
                            label: ui_text!(en = "Parity", zh = "校验位"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!(0),
                                        label: ui_text!(en = "None", zh = "None"),
                                    },
                                    EnumItem {
                                        key: json!(1),
                                        label: ui_text!(en = "Odd", zh = "Odd"),
                                    },
                                    EnumItem {
                                        key: json!(2),
                                        label: ui_text!(en = "Even", zh = "Even"),
                                    },
                                ],
                            },
                            default_value: Some(json!(0)),
                            order: Some(5),
                            ui: None,
                            rules: None,
                            when: None,
                        })),
                    ],
                },
            ],
        }),
        // DNP3 Specific
        Node::Group(Group {
            id: "dnp3".into(),
            label: ui_text!(en = "DNP3 Settings", zh = "DNP3 设置"),
            description: None,
            collapsible: false,
            order: Some(10),
            children: vec![
                Node::Field(Box::new(Field {
                    path: "localAddr".into(),
                    label: ui_text!(en = "Master Address", zh = "主站地址"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(1)),
                    order: Some(1),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(
                                en = "Master address is required",
                                zh = "主站地址是必填项"
                            )),
                        }),
                        min: Some(RuleValue::WithMessage {
                            value: 0.0,
                            message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                        }),
                        max: Some(RuleValue::WithMessage {
                            value: 65519.0,
                            message: Some(ui_text!(en = "Max 65519", zh = "最大为 65519")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "remoteAddr".into(),
                    label: ui_text!(en = "Outstation Address", zh = "从站地址"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(1024)),
                    order: Some(2),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(
                                en = "Outstation address is required",
                                zh = "从站地址是必填项"
                            )),
                        }),
                        min: Some(RuleValue::WithMessage {
                            value: 0.0,
                            message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                        }),
                        max: Some(RuleValue::WithMessage {
                            value: 65519.0,
                            message: Some(ui_text!(en = "Max 65519", zh = "最大为 65519")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "responseTimeoutMs".into(),
                    label: ui_text!(en = "Response Timeout", zh = "响应超时"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(5000)),
                    order: Some(3),
                    ui: Some(UiProps {
                        help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::WithMessage {
                            value: 100.0,
                            message: Some(ui_text!(en = "Min 100", zh = "最小为 100")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "integrityScanIntervalMs".into(),
                    label: ui_text!(en = "Integrity Scan Interval", zh = "总召间隔"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(20000)),
                    order: Some(5),
                    ui: Some(UiProps {
                        help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(
                                en = "Integrity scan interval is required",
                                zh = "总召间隔为必填项"
                            )),
                        }),
                        min: Some(RuleValue::WithMessage {
                            value: 100.0,
                            message: Some(ui_text!(en = "Min 100 ms", zh = "最小值 100 毫秒")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "eventScanIntervalMs".into(),
                    label: ui_text!(en = "Event Scan Interval", zh = "事件扫描间隔"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(1000)),
                    order: Some(6),
                    ui: Some(UiProps {
                        help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(
                                en = "Event scan interval is required",
                                zh = "事件扫描间隔为必填项"
                            )),
                        }),
                        min: Some(RuleValue::WithMessage {
                            value: 0.0,
                            message: Some(ui_text!(en = "Must be >= 0", zh = "必须大于等于 0")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
            ],
        }),
    ]
}

/// Build device-level configuration nodes for the DNP3 driver.
fn build_device_nodes() -> Vec<Node> {
    vec![]
}

/// Build point-level configuration nodes for the DNP3 driver.
fn build_point_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "group".into(),
            label: ui_text!(en = "Object Group", zh = "对象组"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(Dnp3PointGroup::BinaryInput as u8),
                        label: ui_text!(en = "Binary Input", zh = "开关量输入"),
                    },
                    EnumItem {
                        key: json!(Dnp3PointGroup::DoubleBitBinaryInput as u8),
                        label: ui_text!(en = "Double Bit Binary Input", zh = "双点开关量输入"),
                    },
                    EnumItem {
                        key: json!(Dnp3PointGroup::BinaryOutput as u8),
                        label: ui_text!(en = "Binary Output", zh = "开关量输出"),
                    },
                    EnumItem {
                        key: json!(Dnp3PointGroup::Counter as u8),
                        label: ui_text!(en = "Counter", zh = "计数器"),
                    },
                    EnumItem {
                        key: json!(Dnp3PointGroup::AnalogInput as u8),
                        label: ui_text!(en = "Analog Input", zh = "模拟量输入"),
                    },
                    EnumItem {
                        key: json!(Dnp3PointGroup::AnalogOutput as u8),
                        label: ui_text!(en = "Analog Output", zh = "模拟量输出"),
                    },
                    EnumItem {
                        key: json!(Dnp3PointGroup::OctetString as u8),
                        label: ui_text!(en = "Octet String", zh = "八位字节串"),
                    },
                ],
            },
            default_value: None,
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Group is required", zh = "对象组是必填项")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "index".into(),
            label: ui_text!(en = "Index", zh = "索引"),
            data_type: UiDataType::Integer,
            default_value: None,
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Index is required", zh = "索引是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        // CROB modeling knobs for BinaryOutput points
        Node::Field(Box::new(Field {
            path: "crobCount".into(),
            label: ui_text!(en = "CROB Count", zh = "CROB次数"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(1)),
            order: Some(50),
            ui: Some(UiProps {
                placeholder: Some(ui_text!(en = "Default 1", zh = "默认 1")),
                ..Default::default()
            }),
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: Some(ui_text!(en = "Min 1", zh = "最小为 1")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 255.0,
                    message: Some(ui_text!(en = "Max 255", zh = "最大为 255")),
                }),
                ..Default::default()
            }),
            when: Some(vec![When {
                target: "group".into(),
                operator: Operator::Eq,
                value: json!(Dnp3PointGroup::BinaryOutput as u8),
                effect: WhenEffect::If,
            }]),
        })),
        Node::Field(Box::new(Field {
            path: "crobOnTimeMs".into(),
            label: ui_text!(en = "CROB On Time", zh = "CROB OnTime"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(0)),
            order: Some(51),
            ui: Some(UiProps {
                placeholder: Some(ui_text!(en = "Default 0", zh = "默认 0")),
                ..Default::default()
            }),
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 4_294_967_295.0,
                    message: Some(ui_text!(en = "Max 4294967295", zh = "最大为 4294967295")),
                }),
                ..Default::default()
            }),
            when: Some(vec![When {
                target: "group".into(),
                operator: Operator::Eq,
                value: json!(Dnp3PointGroup::BinaryOutput as u8),
                effect: WhenEffect::If,
            }]),
        })),
        Node::Field(Box::new(Field {
            path: "crobOffTimeMs".into(),
            label: ui_text!(en = "CROB Off Time (ms)", zh = "CROB OffTime(ms)"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(0)),
            order: Some(52),
            ui: Some(UiProps {
                placeholder: Some(ui_text!(en = "Default 0", zh = "默认 0")),
                ..Default::default()
            }),
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 4_294_967_295.0,
                    message: Some(ui_text!(en = "Max 4294967295", zh = "最大为 4294967295")),
                }),
                ..Default::default()
            }),
            when: Some(vec![When {
                target: "group".into(),
                operator: Operator::Eq,
                value: json!(Dnp3PointGroup::BinaryOutput as u8),
                effect: WhenEffect::If,
            }]),
        })),
    ]
}

/// Build action-level configuration nodes for the DNP3 driver.
fn build_action_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "group".into(),
            label: ui_text!(en = "Command Type", zh = "指令类型"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(Dnp3CommandType::CROB as u8),
                        label: ui_text!(en = "CROB", zh = "控制中继输出块"),
                    },
                    EnumItem {
                        key: json!(Dnp3CommandType::AnalogOutputCommand as u8),
                        label: ui_text!(en = "Analog Output Command", zh = "模拟量输出指令"),
                    },
                    EnumItem {
                        key: json!(Dnp3CommandType::ColdRestart as u8),
                        label: ui_text!(en = "Cold Restart", zh = "冷重启"),
                    },
                    EnumItem {
                        key: json!(Dnp3CommandType::WarmRestart as u8),
                        label: ui_text!(en = "Warm Restart", zh = "热重启"),
                    },
                ],
            },
            default_value: Some(json!(Dnp3CommandType::CROB as u8)),
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Command type is required",
                        zh = "指令类型是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "index".into(),
            label: ui_text!(en = "Index", zh = "索引"),
            data_type: UiDataType::Integer,
            default_value: None,
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Index is required", zh = "索引是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        // CROB tuning knobs (per input parameter)
        Node::Field(Box::new(Field {
            path: "crobCount".into(),
            label: ui_text!(en = "CROB Count", zh = "CROB 次数(count)"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(1)),
            order: Some(3),
            ui: Some(UiProps {
                placeholder: Some(ui_text!(en = "Default 1", zh = "默认 1")),
                ..Default::default()
            }),
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: Some(ui_text!(en = "Min 1", zh = "最小为 1")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 255.0,
                    message: Some(ui_text!(en = "Max 255", zh = "最大为 255")),
                }),
                ..Default::default()
            }),
            when: Some(vec![When {
                target: "group".into(),
                operator: Operator::Eq,
                value: json!(Dnp3CommandType::CROB as u8),
                effect: WhenEffect::If,
            }]),
        })),
        Node::Field(Box::new(Field {
            path: "crobOnTimeMs".into(),
            label: ui_text!(en = "CROB On Time (ms)", zh = "CROB OnTime(ms)"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(0)),
            order: Some(4),
            ui: Some(UiProps {
                placeholder: Some(ui_text!(en = "Default 0", zh = "默认 0")),
                ..Default::default()
            }),
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 4_294_967_295.0,
                    message: Some(ui_text!(en = "Max 4294967295", zh = "最大为 4294967295")),
                }),
                ..Default::default()
            }),
            when: Some(vec![When {
                target: "group".into(),
                operator: Operator::Eq,
                value: json!(Dnp3CommandType::CROB as u8),
                effect: WhenEffect::If,
            }]),
        })),
        Node::Field(Box::new(Field {
            path: "crobOffTimeMs".into(),
            label: ui_text!(en = "CROB Off Time (ms)", zh = "CROB OffTime(ms)"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(0)),
            order: Some(5),
            ui: Some(UiProps {
                placeholder: Some(ui_text!(en = "Default 0", zh = "默认 0")),
                ..Default::default()
            }),
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(en = "Min 0", zh = "最小为 0")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 4_294_967_295.0,
                    message: Some(ui_text!(en = "Max 4294967295", zh = "最大为 4294967295")),
                }),
                ..Default::default()
            }),
            when: Some(vec![When {
                target: "group".into(),
                operator: Operator::Eq,
                value: json!(Dnp3CommandType::CROB as u8),
                effect: WhenEffect::If,
            }]),
        })),
    ]
}
