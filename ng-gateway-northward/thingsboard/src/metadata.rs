use ng_gateway_sdk::{
    ui_text, EnumItem, Field, Group, Node, PluginConfigSchemas, RuleValue, Rules, UiDataType,
    Union, UnionCase,
};
use serde_json::json;

/// Build static metadata once to be embedded as JSON for the gateway UI/config
pub(super) fn build_metadata() -> PluginConfigSchemas {
    vec![
        // Connection group
        Node::Group(Group {
            id: "connection".into(),
            label: ui_text!(en = "Connection", zh = "连接"),
            description: None,
            collapsible: false,
            order: Some(1),
            children: vec![
                // Mode selector matches serde(tag = \"mode\") of ConnectionConfig
                Node::Field(Box::new(Field {
                    path: "connection.mode".into(),
                    label: ui_text!(en = "Mode", zh = "模式"),
                    data_type: UiDataType::Enum {
                        items: vec![
                            EnumItem { key: json!("none"), label: ui_text!(en = "No Auth", zh = "无认证") },
                            EnumItem { key: json!("username_password"), label: ui_text!(en = "Username / Password", zh = "用户名/密码") },
                            EnumItem { key: json!("token"), label: ui_text!(en = "Access Token", zh = "访问令牌") },
                            EnumItem { key: json!("x509_certificate"), label: ui_text!(en = "X.509 Certificate", zh = "X.509证书") },
                            EnumItem { key: json!("provision"), label: ui_text!(en = "Provision", zh = "注册") },
                        ],
                    },
                    default_value: Some(json!("token")),
                    order: Some(1),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(en = "Mode is required", zh = "模式是必填项")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                // Per-mode fields
                Node::Union(Union {
                    order: Some(2),
                    discriminator: "connection.mode".into(),
                    mapping: vec![
                        // None
                        UnionCase {
                            case_value: json!("none"),
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
                                            message: Some(ui_text!(en = "Host is required", zh = "主机是必填项")),
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
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.port".into(),
                                    label: ui_text!(en = "Port", zh = "端口"),
                                    data_type: UiDataType::Integer,
                                    default_value: Some(json!(1883)),
                                    order: Some(2),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Port is required", zh = "端口是必填项")),
                                        }),
                                        min: Some(RuleValue::WithMessage { value: 1.0, message: None }),
                                        max: Some(RuleValue::WithMessage { value: 65535.0, message: None }),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.client_id".into(),
                                    label: ui_text!(en = "Client ID", zh = "客户端ID"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(3),
                                    ui: None,
                                    rules: None,
                                    when: None,
                                })),
                            ],
                        },
                        // Username/Password
                        UnionCase {
                            case_value: json!("username_password"),
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
                                            message: Some(ui_text!(en = "Host is required", zh = "主机是必填项")),
                                        }),
                                        pattern: Some(RuleValue::WithMessage {
                                            value: "^(?:(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)(?:\\.(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?))*|(?:(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)\\.){3}(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d))$".to_string(),
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
                                    path: "connection.port".into(),
                                    label: ui_text!(en = "Port", zh = "端口"),
                                    data_type: UiDataType::Integer,
                                    default_value: Some(json!(1883)),
                                    order: Some(2),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Port is required", zh = "端口是必填项")),
                                        }),
                                        min: Some(RuleValue::Value(1.0)),
                                        max: Some(RuleValue::Value(65535.0)),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.client_id".into(),
                                    label: ui_text!(en = "Client ID", zh = "客户端ID"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(3),
                                    ui: None,
                                    rules: None,
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.username".into(),
                                    label: ui_text!(en = "Username", zh = "用户名"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(4),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Username is required", zh = "用户名是必填项")),
                                        }),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.password".into(),
                                    label: ui_text!(en = "Password", zh = "密码"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(5),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Password is required", zh = "密码是必填项")),
                                        }),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                            ],
                        },
                        // Token
                        UnionCase {
                            case_value: json!("token"),
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
                                            message: Some(ui_text!(en = "Host is required", zh = "主机是必填项")),
                                        }),
                                        pattern: Some(RuleValue::WithMessage {
                                            value: "^(?:(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)(?:\\.(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?))*|(?:(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)\\.){3}(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d))$".to_string(),
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
                                    path: "connection.port".into(),
                                    label: ui_text!(en = "Port", zh = "端口"),
                                    data_type: UiDataType::Integer,
                                    default_value: Some(json!(1883)),
                                    order: Some(2),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Port is required", zh = "端口是必填项")),
                                        }),
                                        min: Some(RuleValue::Value(1.0)),
                                        max: Some(RuleValue::Value(65535.0)),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.client_id".into(),
                                    label: ui_text!(en = "Client ID", zh = "客户端ID"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(3),
                                    ui: None,
                                    rules: None,
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.access_token".into(),
                                    label: ui_text!(en = "Access Token", zh = "访问令牌"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(4),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Access Token is required", zh = "访问令牌是必填项")),
                                        }),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                            ],
                        },
                        // X.509 Certificate
                        UnionCase {
                            case_value: json!("x509_certificate"),
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
                                            message: Some(ui_text!(en = "Host is required", zh = "主机是必填项")),
                                        }),
                                        pattern: Some(RuleValue::WithMessage {
                                            value: "^(?:(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)(?:\\.(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?))*|(?:(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)\\.){3}(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d))$".to_string(),
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
                                    path: "connection.tls_port".into(),
                                    label: ui_text!(en = "TLS Port", zh = "TLS端口(X.509)"),
                                    data_type: UiDataType::Integer,
                                    default_value: Some(json!(8883)),
                                    order: Some(2),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "TLS Port is required", zh = "TLS端口是必填项")),
                                        }),
                                        min: Some(RuleValue::Value(1.0)),
                                        max: Some(RuleValue::Value(65535.0)),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.client_id".into(),
                                    label: ui_text!(en = "Client ID", zh = "客户端ID"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(3),
                                    ui: None,
                                    rules: None,
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.cert_path".into(),
                                    label: ui_text!(en = "Certificate Path (PEM)", zh = "证书"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(4),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Certificate is required", zh = "证书是必填项")),
                                        }),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.private_key_path".into(),
                                    label: ui_text!(en = "Private Key Path (PEM)", zh = "私钥"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(5),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Private Key is required", zh = "私钥是必填项")),
                                        }),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                            ],
                        },
                        // Provision
                        UnionCase {
                            case_value: json!("provision"),
                            children: vec![
                                // Provision method and hint on ports
                                Node::Field(Box::new(Field {
                                    path: "connection.provision_method".into(),
                                    label: ui_text!(en = "Provision Method", zh = "注册方式"),
                                    data_type: UiDataType::Enum {
                                        items: vec![
                                            EnumItem {
                                                key: json!("ACCESS_TOKEN"), 
                                                label: ui_text!(en = "Access Token", zh = "访问令牌") 
                                            },
                                            EnumItem {
                                                key: json!("MQTT_BASIC"), 
                                                label: ui_text!(en = "Username / Password", zh = "用户名/密码") 
                                            },
                                            EnumItem {
                                                key: json!("X509_CERTIFICATE"), 
                                                label: ui_text!(en = "X.509 Certificate", zh = "X.509证书") 
                                            },
                                        ],
                                    },
                                    default_value: Some(json!("ACCESS_TOKEN")),
                                    order: Some(1),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Provision Method is required", zh = "注册方式是必填项")),
                                        }),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.host".into(),
                                    label: ui_text!(en = "Provision Host", zh = "注册主机"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(2),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Provision Host is required", zh = "注册主机是必填项")),
                                        }),
                                        pattern: Some(RuleValue::WithMessage {
                                            value: "^(?:(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)(?:\\.(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?))*|(?:(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)\\.){3}(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d))$".to_string(),
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
                                    path: "connection.port".into(),
                                    label: ui_text!(en = "Port", zh = "端口"),
                                    data_type: UiDataType::Integer,
                                    default_value: Some(json!(1883)),
                                    order: Some(3),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Port is required", zh = "端口是必填项")),
                                        }),
                                        min: Some(RuleValue::Value(1.0)),
                                        max: Some(RuleValue::Value(65535.0)),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.tls_port".into(),
                                    label: ui_text!(en = "TLS Port", zh = "TLS端口"),
                                    data_type: UiDataType::Integer,
                                    default_value: Some(json!(8883)),
                                    order: Some(4),
                                    ui: None,
                                    rules: Some(Rules {
                                        min: Some(RuleValue::Value(1.0)),
                                        max: Some(RuleValue::Value(65535.0)),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.timeout_ms".into(),
                                    label: ui_text!(en = "Request Timeout", zh = "请求超时"),
                                    data_type: UiDataType::Integer,
                                    default_value: Some(json!(30000)),
                                    order: Some(5),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Request Timeout is required", zh = "请求超时是必填项")),
                                        }),
                                        min: Some(RuleValue::Value(1000.0)),
                                        max: Some(RuleValue::Value(600000.0)),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.max_retries".into(),
                                    label: ui_text!(en = "Max Retries", zh = "最大重试次数"),
                                    data_type: UiDataType::Integer,
                                    default_value: Some(json!(3)),
                                    order: Some(6),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Max Retries is required", zh = "最大重试次数是必填项")),
                                        }),
                                        min: Some(RuleValue::Value(0.0)),
                                        max: Some(RuleValue::Value(1000.0)),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.retry_delay_ms".into(),
                                    label: ui_text!(en = "Retry Delay", zh = "重试间隔"),
                                    data_type: UiDataType::Integer,
                                    default_value: Some(json!(10000)),
                                    order: Some(7),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Retry Delay is required", zh = "重试间隔是必填项")),
                                        }),
                                        min: Some(RuleValue::Value(100.0)),
                                        max: Some(RuleValue::Value(600000.0)),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.provision_device_key".into(),
                                    label: ui_text!(en = "Provision Key", zh = "注册Key"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(8),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Provision Device Key is required", zh = "注册设备Key是必填项")),
                                        }),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                                Node::Field(Box::new(Field {
                                    path: "connection.provision_device_secret".into(),
                                    label: ui_text!(en = "Provision Secret", zh = "注册Secret"),
                                    data_type: UiDataType::String,
                                    default_value: None,
                                    order: Some(9),
                                    ui: None,
                                    rules: Some(Rules {
                                        required: Some(RuleValue::WithMessage {
                                            value: true,
                                            message: Some(ui_text!(en = "Provision Secret is required", zh = "注册Secret是必填项")),
                                        }),
                                        ..Default::default()
                                    }),
                                    when: None,
                                })),
                            ],
                        },
                    ],
                }),
            ],
        }),
        // Communication group
        Node::Group(Group {
            id: "communication".into(),
            label: ui_text!(en = "Communication", zh = "通信"),
            description: None,
            collapsible: false,
            order: Some(2),
            children: vec![
                Node::Field(Box::new(Field {
                    path: "communication.message_format".into(),
                    label: ui_text!(en = "Message Format", zh = "消息格式"),
                    data_type: UiDataType::Enum {
                        items: vec![
                            EnumItem {
                                key: json!("json"),
                                label: ui_text!(en = "JSON", zh = "JSON")
                            },
                            EnumItem {
                                key: json!("protobuf"),
                                label: ui_text!(en = "Protobuf", zh = "Protobuf")
                            },
                        ],
                    },
                    default_value: Some(json!("json")),
                    order: Some(1),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(en = "Message Format is required", zh = "消息格式是必填项")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "communication.qos".into(),
                    label: ui_text!(en = "QoS", zh = "QoS"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(1)),
                    order: Some(2),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(en = "QoS is required", zh = "QoS是必填项")),
                        }),
                        min: Some(RuleValue::Value(0.0)),
                        max: Some(RuleValue::Value(2.0)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "communication.retain_messages".into(),
                    label: ui_text!(en = "Retain Messages", zh = "保留消息"),
                    data_type: UiDataType::Boolean,
                    default_value: Some(json!(false)),
                    order: Some(3),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(en = "Retain Messages is required", zh = "保留消息是必填项")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "communication.keep_alive".into(),
                    label: ui_text!(en = "Keep Alive", zh = "保活"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(60)),
                    order: Some(4),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(en = "Keep Alive is required", zh = "保活是必填项")),
                        }),
                        min: Some(RuleValue::Value(5.0)),
                        max: Some(RuleValue::Value(3600.0)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "communication.clean_session".into(),
                    label: ui_text!(en = "Clean Session", zh = "清理会话"),
                    data_type: UiDataType::Boolean,
                    default_value: Some(json!(false)),
                    order: Some(5),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::WithMessage {
                            value: true,
                            message: Some(ui_text!(en = "Clean Session is required", zh = "清理会话是必填项")),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
            ],
        }),
    ]
}
