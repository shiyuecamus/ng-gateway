use ng_gateway_sdk::{
    ui_text, DriverSchemas, EnumItem, Field, Node, RuleValue, Rules, UiDataType, UiProps, Union,
    UnionCase,
};
use serde_json::json;

use crate::types::{OpcUaReadMode, SecurityMode, SecurityPolicy};

/// Build static metadata once to be embedded as JSON for the gateway UI/config
pub(super) fn build_metadata() -> DriverSchemas {
    DriverSchemas {
        channel: build_channel_nodes(),
        device: build_device_nodes(),
        point: build_point_nodes(),
        action: build_action_nodes(),
    }
}

/// Build channel-level configuration nodes for the OPC UA driver.
fn build_channel_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "applicationName".into(),
            label: ui_text!(en = "Application Name", zh = "应用名称"),
            data_type: UiDataType::String,
            default_value: None,
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Application Name is required",
                        zh = "应用名称是必填项"
                    )),
                }),
                min_length: Some(RuleValue::WithMessage {
                    value: 1,
                    message: Some(ui_text!(en = "Must not be empty", zh = "不能为空")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "applicationUri".into(),
            label: ui_text!(en = "Application URI", zh = "应用 URI"),
            data_type: UiDataType::String,
            default_value: None,
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Application URI is required",
                        zh = "应用 URI 是必填项"
                    )),
                }),
                min_length: Some(RuleValue::WithMessage {
                    value: 1,
                    message: Some(ui_text!(en = "Must not be empty", zh = "不能为空")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "url".into(),
            label: ui_text!(en = "Endpoint URL", zh = "服务器地址"),
            data_type: UiDataType::String,
            default_value: None,
            order: Some(3),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "URL is required", zh = "地址是必填项")),
                }),
                // Basic URL check (schema://host:port/path?)
                pattern: Some(RuleValue::WithMessage {
                    value: "^(opc.tcp|http|https)://.+$".to_string(),
                    message: Some(ui_text!(en = "Enter a valid URL", zh = "请输入有效的 URL")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        // Auth selector
        Node::Field(Box::new(Field {
            path: "auth.kind".into(),
            label: ui_text!(en = "Auth Type", zh = "认证方式"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!("anonymous"),
                        label: ui_text!(en = "Anonymous", zh = "匿名"),
                    },
                    EnumItem {
                        key: json!("userPassword"),
                        label: ui_text!(en = "User & Password", zh = "用户名密码"),
                    },
                    EnumItem {
                        key: json!("issuedToken"),
                        label: ui_text!(en = "Issued Token", zh = "颁发令牌"),
                    },
                    EnumItem {
                        key: json!("certificate"),
                        label: ui_text!(en = "Certificate", zh = "证书"),
                    },
                ],
            },
            default_value: Some(json!("anonymous")),
            order: Some(4),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: None,
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Union(Union {
            order: Some(5),
            discriminator: "auth.kind".into(),
            mapping: vec![
                UnionCase {
                    case_value: json!("anonymous"),
                    children: vec![],
                },
                UnionCase {
                    case_value: json!("userPassword"),
                    children: vec![
                        Node::Field(Box::new(Field {
                            path: "auth.username".into(),
                            label: ui_text!(en = "Username", zh = "用户名"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(1),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: None,
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "auth.password".into(),
                            label: ui_text!(en = "Password", zh = "密码"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(2),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: None,
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                    ],
                },
                UnionCase {
                    case_value: json!("issuedToken"),
                    children: vec![Node::Field(Box::new(Field {
                        path: "auth.token".into(),
                        label: ui_text!(en = "Issued Token", zh = "颁发令牌"),
                        data_type: UiDataType::String,
                        default_value: None,
                        order: Some(1),
                        ui: None,
                        rules: Some(Rules {
                            required: Some(RuleValue::WithMessage {
                                value: true,
                                message: None,
                            }),
                            ..Default::default()
                        }),
                        when: None,
                    }))],
                },
                UnionCase {
                    case_value: json!("certificate"),
                    children: vec![
                        Node::Field(Box::new(Field {
                            path: "auth.privateKey".into(),
                            label: ui_text!(en = "Private Key", zh = "私钥"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(1),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: None,
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "auth.certificate".into(),
                            label: ui_text!(en = "Certificate", zh = "证书"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(2),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: None,
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                    ],
                },
            ],
        }),
        // Security
        Node::Field(Box::new(Field {
            path: "securityPolicy".into(),
            label: ui_text!(en = "Security Policy", zh = "安全策略"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(SecurityPolicy::None as u16),
                        label: ui_text!(en = "None", zh = "无"),
                    },
                    EnumItem {
                        key: json!(SecurityPolicy::Basic128Rsa15 as u16),
                        label: ui_text!(en = "Basic128Rsa15", zh = "Basic128Rsa15"),
                    },
                    EnumItem {
                        key: json!(SecurityPolicy::Basic256 as u16),
                        label: ui_text!(en = "Basic256", zh = "Basic256"),
                    },
                    EnumItem {
                        key: json!(SecurityPolicy::Basic256Sha256 as u16),
                        label: ui_text!(en = "Basic256Sha256", zh = "Basic256Sha256"),
                    },
                    EnumItem {
                        key: json!(SecurityPolicy::Aes128Sha256RsaOaep as u16),
                        label: ui_text!(en = "Aes128Sha256RsaOaep", zh = "Aes128Sha256RsaOaep"),
                    },
                    EnumItem {
                        key: json!(SecurityPolicy::Aes256Sha256RsaPss as u16),
                        label: ui_text!(en = "Aes256Sha256RsaPss", zh = "Aes256Sha256RsaPss"),
                    },
                    EnumItem {
                        key: json!(SecurityPolicy::Unknown as u16),
                        label: ui_text!(en = "Unknown", zh = "未知"),
                    },
                ],
            },
            default_value: Some(json!(SecurityPolicy::None as u16)),
            order: Some(6),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: None,
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "securityMode".into(),
            label: ui_text!(en = "Security Mode", zh = "安全模式"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(SecurityMode::None as u16),
                        label: ui_text!(en = "None", zh = "无"),
                    },
                    EnumItem {
                        key: json!(SecurityMode::Sign as u16),
                        label: ui_text!(en = "Sign", zh = "签名"),
                    },
                    EnumItem {
                        key: json!(SecurityMode::SignAndEncrypt as u16),
                        label: ui_text!(en = "SignAndEncrypt", zh = "签名加密"),
                    },
                ],
            },
            default_value: Some(json!(SecurityMode::None as u16)),
            order: Some(7),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: None,
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "readMode".into(),
            label: ui_text!(en = "Collection Mode", zh = "采集模式"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!(OpcUaReadMode::Subscribe as u16),
                        label: ui_text!(en = "Subscribe", zh = "订阅"),
                    },
                    EnumItem {
                        key: json!(OpcUaReadMode::Read as u16),
                        label: ui_text!(en = "Periodic Collection", zh = "周期采集"),
                    },
                ],
            },
            default_value: Some(json!(OpcUaReadMode::Subscribe as u16)),
            order: Some(8),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Collection Mode is required",
                        zh = "采集模式是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "sessionTimeout".into(),
            label: ui_text!(en = "Session Timeout", zh = "会话超时"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(30_000)),
            order: Some(9),
            ui: Some(UiProps {
                help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                ..Default::default()
            }),
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 1000.0,
                    message: None,
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "maxFailedKeepAliveCount".into(),
            label: ui_text!(en = "Max Failed Keep Alive Count", zh = "最大失败保活次数"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(3)),
            order: Some(10),
            ui: None,
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: None,
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "keepAliveInterval".into(),
            label: ui_text!(en = "Keep-alive Interval", zh = "保活间隔"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(30_000)),
            order: Some(11),
            ui: Some(UiProps {
                help: Some(ui_text!(en = "Unit：Milliseconds", zh = "单位：毫秒")),
                ..Default::default()
            }),
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 1000.0,
                    message: None,
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "subscribeBatchSize".into(),
            label: ui_text!(en = "Subscribe Batch Size", zh = "订阅批量大小"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(256)),
            order: Some(12),
            ui: None,
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: None,
                }),
                max: Some(RuleValue::WithMessage {
                    value: 1024.0,
                    message: None,
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}

/// Build device-level configuration nodes for the OPC UA driver.
fn build_device_nodes() -> Vec<Node> {
    vec![]
}

/// Build point-level configuration nodes for the OPC UA driver.
fn build_point_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "nodeId".into(),
        label: ui_text!(en = "Node ID", zh = "节点 ID"),
        data_type: UiDataType::String,
        default_value: None,
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: None,
            }),
            min_length: Some(RuleValue::WithMessage {
                value: 1,
                message: None,
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}

/// Build action-level configuration nodes for the OPC UA driver.
fn build_action_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "nodeId".into(),
        label: ui_text!(en = "Node ID", zh = "节点 ID"),
        data_type: UiDataType::String,
        default_value: None,
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: None,
            }),
            min_length: Some(RuleValue::WithMessage {
                value: 1,
                message: None,
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}
