//! Static plugin metadata for UI-driven configuration.
//!
//! The gateway UI consumes this schema to render forms and validate inputs.
//! Keep this module free of runtime side effects.

use ng_gateway_sdk::{
    ui_text, EnumItem, Field, Group, Node, Operator, PluginConfigSchemas, RuleValue, Rules,
    UiDataType, UiProps, UiText, Union, UnionCase, When, WhenEffect,
};
use serde_json::json;

/// Build static metadata once to be embedded as JSON for the gateway UI/config.
pub(super) fn build_metadata() -> PluginConfigSchemas {
    vec![
        build_connection_group(),
        build_uplink_group(),
        build_downlink_group(),
    ]
}

fn build_connection_group() -> Node {
    Node::Group(Group {
        id: "connection".into(),
        label: ui_text!(en = "Connection", zh = "连接"),
        description: None,
        collapsible: false,
        order: Some(1),
        children: vec![
            Node::Field(Box::new(Field {
                path: "connection.bootstrapServers".into(),
                label: ui_text!(en = "Bootstrap Servers", zh = "Broker 地址"),
                data_type: UiDataType::String,
                default_value: Some(json!("127.0.0.1:9092")),
                order: Some(1),
                ui: Some(UiProps {
                    placeholder: Some(ui_text!(
                        en = "host1:9092,host2:9092",
                        zh = "host1:9092,host2:9092"
                    )),
                    help: Some(ui_text!(
                        en = "Comma-separated broker list.",
                        zh = "逗号分隔的 broker 列表。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(en = "Required", zh = "必填")),
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "connection.clientId".into(),
                label: ui_text!(en = "Client ID", zh = "Client ID"),
                data_type: UiDataType::String,
                default_value: None,
                order: Some(2),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Optional. If empty, the plugin uses a stable client id derived from app_id.",
                        zh = "可选。不填写则自动使用 app_id 生成稳定的 client id。"
                    )),
                    ..Default::default()
                }),
                rules: None,
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "connection.security.protocol".into(),
                label: ui_text!(en = "Security Protocol", zh = "安全协议"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!("plaintext"),
                            label: ui_text!(en = "PLAINTEXT", zh = "PLAINTEXT"),
                        },
                        EnumItem {
                            key: json!("ssl"),
                            label: ui_text!(en = "SSL", zh = "SSL"),
                        },
                        EnumItem {
                            key: json!("sasl_plaintext"),
                            label: ui_text!(en = "SASL_PLAINTEXT", zh = "SASL_PLAINTEXT"),
                        },
                        EnumItem {
                            key: json!("sasl_ssl"),
                            label: ui_text!(en = "SASL_SSL", zh = "SASL_SSL"),
                        },
                    ],
                },
                default_value: Some(json!("plaintext")),
                order: Some(3),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Union(Union {
                order: Some(4),
                discriminator: "connection.security.protocol".into(),
                mapping: vec![
                    UnionCase {
                        case_value: json!("plaintext"),
                        children: vec![],
                    },
                    UnionCase {
                        case_value: json!("ssl"),
                        children: vec![build_tls_group(
                            "connection.security.tls",
                            ui_text!(en = "TLS", zh = "TLS"),
                            1,
                            false,
                        )],
                    },
                    UnionCase {
                        case_value: json!("sasl_plaintext"),
                        children: vec![build_sasl_group(
                            "connection.security.sasl",
                            ui_text!(en = "SASL", zh = "SASL"),
                            1,
                            true,
                        )],
                    },
                    UnionCase {
                        case_value: json!("sasl_ssl"),
                        children: vec![
                            build_tls_group(
                                "connection.security.tls",
                                ui_text!(en = "TLS", zh = "TLS"),
                                1,
                                false,
                            ),
                            build_sasl_group(
                                "connection.security.sasl",
                                ui_text!(en = "SASL", zh = "SASL"),
                                2,
                                true,
                            ),
                        ],
                    },
                ],
            }),
        ],
    })
}

fn build_tls_group(prefix: &str, label: UiText, order: i32, required: bool) -> Node {
    let ca_path = format!("{prefix}.caLocation");
    let cert_path = format!("{prefix}.certificateLocation");
    let key_path = format!("{prefix}.keyLocation");
    let key_password_path = format!("{prefix}.keyPassword");
    let endpoint_algo_path = format!("{prefix}.endpointIdentificationAlgorithm");

    let required_rule = Some(Rules {
        required: Some(RuleValue::Value(required)),
        ..Default::default()
    });

    Node::Group(Group {
        id: prefix.into(),
        label,
        description: None,
        collapsible: true,
        order: Some(order),
        children: vec![
            Node::Field(Box::new(Field {
                path: ca_path.clone(),
                label: ui_text!(en = "CA Location", zh = "CA 文件路径"),
                data_type: UiDataType::String,
                default_value: None,
                order: Some(1),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Optional. Path to CA bundle file (PEM). If empty, system trust store may be used.",
                        zh = "可选。CA 证书路径（PEM）。不填写时可能使用系统信任库。"
                    )),
                    ..Default::default()
                }),
                rules: required_rule.clone(),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: cert_path,
                label: ui_text!(en = "Client Certificate", zh = "客户端证书"),
                data_type: UiDataType::String,
                default_value: None,
                order: Some(2),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Optional. Path to client certificate (PEM).",
                        zh = "可选。客户端证书路径（PEM）。"
                    )),
                    ..Default::default()
                }),
                rules: required_rule.clone(),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: key_path,
                label: ui_text!(en = "Client Private Key", zh = "客户端私钥"),
                data_type: UiDataType::String,
                default_value: None,
                order: Some(3),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Optional. Path to client private key (PEM).",
                        zh = "可选。客户端私钥路径（PEM）。"
                    )),
                    ..Default::default()
                }),
                rules: required_rule.clone(),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: key_password_path,
                label: ui_text!(en = "Key Password", zh = "私钥密码"),
                data_type: UiDataType::String,
                default_value: None,
                order: Some(4),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Optional. Password for encrypted private key.",
                        zh = "可选。加密私钥的密码。"
                    )),
                    ..Default::default()
                }),
                rules: required_rule.clone(),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: endpoint_algo_path,
                label: ui_text!(en = "Endpoint Identification", zh = "主机名校验"),
                data_type: UiDataType::String,
                default_value: Some(json!("https")),
                order: Some(5),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Optional. ssl.endpoint.identification.algorithm (e.g. https). Empty string disables hostname verification.",
                        zh = "可选。ssl.endpoint.identification.algorithm（例如 https）。置空可关闭主机名校验。"
                    )),
                    ..Default::default()
                }),
                rules: required_rule,
                when: None,
            })),
        ],
    })
}

fn build_sasl_group(prefix: &str, label: UiText, order: i32, required: bool) -> Node {
    let mech = format!("{prefix}.mechanism");
    let username = format!("{prefix}.username");
    let password = format!("{prefix}.password");

    Node::Group(Group {
        id: prefix.into(),
        label,
        description: None,
        collapsible: true,
        order: Some(order),
        children: vec![
            Node::Field(Box::new(Field {
                path: mech.clone(),
                label: ui_text!(en = "Mechanism", zh = "机制"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!("plain"),
                            label: ui_text!(en = "PLAIN", zh = "PLAIN"),
                        },
                        EnumItem {
                            key: json!("scram_sha256"),
                            label: ui_text!(en = "SCRAM-SHA-256", zh = "SCRAM-SHA-256"),
                        },
                        EnumItem {
                            key: json!("scram_sha512"),
                            label: ui_text!(en = "SCRAM-SHA-512", zh = "SCRAM-SHA-512"),
                        },
                    ],
                },
                default_value: Some(json!("plain")),
                order: Some(1),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(required)),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: username.clone(),
                label: ui_text!(en = "Username", zh = "用户名"),
                data_type: UiDataType::String,
                default_value: None,
                order: Some(2),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(required)),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: password.clone(),
                label: ui_text!(en = "Password", zh = "密码"),
                data_type: UiDataType::String,
                default_value: None,
                order: Some(3),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(required)),
                    ..Default::default()
                }),
                when: None,
            })),
        ],
    })
}

fn build_uplink_group() -> Node {
    Node::Group(Group {
        id: "uplink".into(),
        label: ui_text!(en = "Uplink", zh = "上行"),
        description: Some(ui_text!(
            en = "Gateway -> Kafka mappings by event kind.",
            zh = "按事件类型配置 Gateway -> Kafka 映射。"
        )),
        collapsible: false,
        order: Some(2),
        children: vec![
            Node::Field(Box::new(Field {
                path: "uplink.enabled".into(),
                label: ui_text!(en = "Enabled", zh = "启用"),
                data_type: UiDataType::Boolean,
                default_value: Some(json!(true)),
                order: Some(1),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: None,
            })),
            build_uplink_producer_group(),
            build_uplink_event_group(
                "deviceConnected",
                ui_text!(en = "Device Connected", zh = "设备上线"),
                3,
            ),
            build_uplink_event_group(
                "deviceDisconnected",
                ui_text!(en = "Device Disconnected", zh = "设备离线"),
                4,
            ),
            build_uplink_event_group("telemetry", ui_text!(en = "Telemetry", zh = "遥测"), 5),
            build_uplink_event_group("attributes", ui_text!(en = "Attributes", zh = "属性"), 6),
        ],
    })
}

fn build_uplink_producer_group() -> Node {
    Node::Group(Group {
        id: "uplink.producer".into(),
        label: ui_text!(en = "Producer", zh = "生产者"),
        description: None,
        collapsible: true,
        order: Some(2),
        children: vec![
            Node::Field(Box::new(Field {
                path: "uplink.producer.enableIdempotence".into(),
                label: ui_text!(en = "Idempotence", zh = "幂等发送"),
                data_type: UiDataType::Boolean,
                default_value: Some(json!(true)),
                order: Some(1),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Enable idempotent producer for stronger guarantees.",
                        zh = "启用幂等 producer 以获得更强的可靠性。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.acks".into(),
                label: ui_text!(en = "Acks", zh = "确认策略(acks)"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem { key: json!("none"), label: ui_text!(en = "0", zh = "0") },
                        EnumItem { key: json!("one"), label: ui_text!(en = "1", zh = "1") },
                        EnumItem { key: json!("all"), label: ui_text!(en = "all", zh = "all") },
                    ],
                },
                default_value: Some(json!("all")),
                order: Some(2),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Kafka acks policy. all is recommended for durability.",
                        zh = "Kafka acks 策略。建议 all 以获得更高可靠性。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules { required: Some(RuleValue::Value(true)), ..Default::default() }),
                when: Some(vec![
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.compression".into(),
                label: ui_text!(en = "Compression", zh = "压缩"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem { key: json!("none"), label: ui_text!(en = "none", zh = "none") },
                        EnumItem { key: json!("gzip"), label: ui_text!(en = "gzip", zh = "gzip") },
                        EnumItem { key: json!("snappy"), label: ui_text!(en = "snappy", zh = "snappy") },
                        EnumItem { key: json!("lz4"), label: ui_text!(en = "lz4", zh = "lz4") },
                        EnumItem { key: json!("zstd"), label: ui_text!(en = "zstd", zh = "zstd") },
                    ],
                },
                default_value: Some(json!("lz4")),
                order: Some(3),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.lingerMs".into(),
                label: ui_text!(en = "Linger (ms)", zh = "批量延迟(ms)"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(5)),
                order: Some(4),
                ui: None,
                rules: Some(Rules {
                    min: Some(RuleValue::Value(0.0)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.batchNumMessages".into(),
                label: ui_text!(en = "Batch Num Messages", zh = "批量最大消息数"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(1000)),
                order: Some(5),
                ui: None,
                rules: Some(Rules { min: Some(RuleValue::Value(1.0)), ..Default::default() }),
                when: Some(vec![
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.batchSizeBytes".into(),
                label: ui_text!(en = "Batch Size (bytes)", zh = "批量大小(bytes)"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(131072)),
                order: Some(6),
                ui: None,
                rules: Some(Rules { min: Some(RuleValue::Value(1.0)), ..Default::default() }),
                when: Some(vec![
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.messageTimeoutMs".into(),
                label: ui_text!(en = "Message Timeout (ms)", zh = "消息超时(ms)"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(30000)),
                order: Some(7),
                ui: None,
                rules: Some(Rules { min: Some(RuleValue::Value(1.0)), ..Default::default() }),
                when: Some(vec![
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.requestTimeoutMs".into(),
                label: ui_text!(en = "Request Timeout (ms)", zh = "请求超时(ms)"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(10000)),
                order: Some(8),
                ui: None,
                rules: Some(Rules { min: Some(RuleValue::Value(1.0)), ..Default::default() }),
                when: Some(vec![
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.maxInflight".into(),
                label: ui_text!(en = "Max Inflight", zh = "最大并发请求"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(5)),
                order: Some(9),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "max.in.flight.requests.per.connection. Affects ordering when retries happen.",
                        zh = "max.in.flight.requests.per.connection。重试时可能影响有序性。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules { min: Some(RuleValue::Value(1.0)), ..Default::default() }),
                when: Some(vec![
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: "uplink.enabled".into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
        ],
    })
}

fn build_uplink_event_group(group_key: &str, label: UiText, order: i32) -> Node {
    let uplink_enabled_path = "uplink.enabled";
    let enabled_path = format!("uplink.{group_key}.enabled");
    let topic_path = format!("uplink.{group_key}.topic");
    let key_path = format!("uplink.{group_key}.key");
    let mode_path = format!("uplink.{group_key}.payload.mode");
    let payload_mapped_config_path = format!("uplink.{group_key}.payload.config");

    let placeholder_help = ui_text!(
        en = "Handlebars vars: {{app_id}} {{app_name}} {{plugin_type}} {{event_kind}} {{ts_ms}} {{device_id}} {{device_name}} {{device_type}} {{channel_name}}",
        zh = "Handlebars 变量：{{app_id}} {{app_name}} {{plugin_type}} {{event_kind}} {{ts_ms}} {{device_id}} {{device_name}} {{device_type}} {{channel_name}}"
    );

    // Only telemetry/attributes support kv / timeseries_rows payloads.
    let allow_kv_ts_rows = matches!(group_key, "telemetry" | "attributes");

    let mut payload_mode_items: Vec<EnumItem> = vec![EnumItem {
        key: json!("envelope_json"),
        label: ui_text!(en = "envelope_json", zh = "envelope_json"),
    }];
    if allow_kv_ts_rows {
        payload_mode_items.extend([
            EnumItem {
                key: json!("kv"),
                label: ui_text!(en = "kv", zh = "kv"),
            },
            EnumItem {
                key: json!("timeseries_rows"),
                label: ui_text!(en = "timeseries_rows", zh = "timeseries_rows"),
            },
        ]);
    }
    payload_mode_items.push(EnumItem {
        key: json!("mapped_json"),
        label: ui_text!(en = "mapped_json", zh = "mapped_json"),
    });

    let mut payload_union_cases: Vec<UnionCase> = vec![UnionCase {
        case_value: json!("envelope_json"),
        children: vec![],
    }];
    if allow_kv_ts_rows {
        let include_meta_kv_path = format!("uplink.{group_key}.payload.includeMeta");
        payload_union_cases.extend([
            UnionCase {
                case_value: json!("kv"),
                children: vec![Node::Field(Box::new(Field {
                    path: include_meta_kv_path.clone(),
                    label: ui_text!(en = "Include DataType", zh = "附带DataType"),
                    data_type: UiDataType::Boolean,
                    default_value: Some(json!(false)),
                    order: Some(1),
                    ui: Some(UiProps {
                        help: Some(ui_text!(
                            en = "When enabled, kv values become objects: values.{key} = { value, data_type }.",
                            zh = "开启后，kv 的 values 会变为对象：values.{key} = { value, data_type }。"
                        )),
                        ..Default::default()
                    }),
                    rules: Some(Rules { required: Some(RuleValue::Value(true)), ..Default::default() }),
                    when: Some(vec![
                        When { target: uplink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                        When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                        When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                        When { target: uplink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                    ]),
                }))],
            },
            UnionCase {
                case_value: json!("timeseries_rows"),
                children: vec![Node::Field(Box::new(Field {
                    path: include_meta_kv_path.clone(),
                    label: ui_text!(en = "Include DataType", zh = "附带DataType"),
                    data_type: UiDataType::Boolean,
                    default_value: Some(json!(false)),
                    order: Some(1),
                    ui: Some(UiProps {
                        help: Some(ui_text!(
                            en = "When enabled, each row includes DataType.",
                            zh = "开启后，每一行会附带DataType。"
                        )),
                        ..Default::default()
                    }),
                    rules: Some(Rules { required: Some(RuleValue::Value(true)), ..Default::default() }),
                    when: Some(vec![
                        When { target: uplink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                        When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                        When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                        When { target: uplink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                    ]),
                }))],
            },
        ]);
    }
    payload_union_cases.push(UnionCase {
        case_value: json!("mapped_json"),
        children: vec![Node::Field(Box::new(Field {
            path: payload_mapped_config_path.clone(),
            label: ui_text!(en = "Mapped Fields", zh = "映射字段"),
            data_type: UiDataType::Any,
            default_value: Some(json!({})),
            order: Some(1),
            ui: Some(UiProps {
                help: Some(ui_text!(
                    en = "JSON object mapping: out_path -> expression",
                    zh = "JSON 对象映射：out_path -> expression"
                )),
                ..Default::default()
            }),
            rules: Some(Rules {
                required: Some(RuleValue::Value(true)),
                ..Default::default()
            }),
            when: Some(vec![
                When {
                    target: uplink_enabled_path.into(),
                    operator: Operator::Eq,
                    value: json!(true),
                    effect: WhenEffect::Visible,
                },
                When {
                    target: enabled_path.clone(),
                    operator: Operator::Eq,
                    value: json!(true),
                    effect: WhenEffect::Visible,
                },
                When {
                    target: enabled_path.clone(),
                    operator: Operator::Eq,
                    value: json!(false),
                    effect: WhenEffect::Invisible,
                },
                When {
                    target: uplink_enabled_path.into(),
                    operator: Operator::Eq,
                    value: json!(false),
                    effect: WhenEffect::Invisible,
                },
            ]),
        }))],
    });

    Node::Group(Group {
        id: format!("uplink.{group_key}"),
        label,
        description: Some(ui_text!(
            en = "event_kind is fixed by the mapping slot.",
            zh = "event_kind 由映射槽位固定决定。"
        )),
        collapsible: true,
        order: Some(order),
        children: vec![
            Node::Field(Box::new(Field {
                path: enabled_path.clone(),
                label: ui_text!(en = "Enabled", zh = "启用"),
                data_type: UiDataType::Boolean,
                default_value: Some(json!(false)),
                order: Some(1),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: uplink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: uplink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: topic_path.clone(),
                label: ui_text!(en = "Topic Template", zh = "Topic 模板"),
                data_type: UiDataType::String,
                default_value: Some(json!("ng.uplink.{{event_kind}}.{{device_name}}")),
                order: Some(2),
                ui: Some(UiProps {
                    help: Some(placeholder_help.clone()),
                    ..Default::default()
                }),
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(en = "Topic is required", zh = "Topic 必填")),
                    }),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: uplink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: enabled_path.clone(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: enabled_path.clone(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Require,
                    },
                    When {
                        target: enabled_path.clone(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                    When {
                        target: uplink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: key_path.clone(),
                label: ui_text!(en = "Message Key Template", zh = "消息 Key 模板"),
                data_type: UiDataType::String,
                default_value: Some(json!("{{device_id}}")),
                order: Some(3),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Kafka record key. Default keeps per-device ordering.",
                        zh = "Kafka record key。默认保证同设备有序。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: uplink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: enabled_path.clone(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: enabled_path.clone(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                    When {
                        target: uplink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: mode_path.clone(),
                label: ui_text!(en = "Payload Mode", zh = "Payload 模式"),
                data_type: UiDataType::Enum {
                    items: payload_mode_items,
                },
                default_value: Some(json!("envelope_json")),
                order: Some(4),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: uplink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: enabled_path.clone(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: enabled_path.clone(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                    When {
                        target: uplink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Union(Union {
                order: Some(5),
                discriminator: mode_path.clone(),
                mapping: payload_union_cases,
            }),
        ],
    })
}

fn build_downlink_group() -> Node {
    Node::Group(Group {
        id: "downlink".into(),
        label: ui_text!(en = "Downlink", zh = "下行"),
        description: Some(ui_text!(
            en = "Kafka -> Gateway. Enable downlink to start subscription.",
            zh = "Kafka -> Gateway：启用下行后才会启动订阅。"
        )),
        collapsible: true,
        order: Some(3),
        children: vec![
            Node::Field(Box::new(Field {
                path: "downlink.enabled".into(),
                label: ui_text!(en = "Enabled", zh = "启用"),
                data_type: UiDataType::Boolean,
                default_value: Some(json!(true)),
                order: Some(1),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: None,
            })),
            build_downlink_event_group("writePoint", ui_text!(en = "Write Point", zh = "写点"), 3),
            build_downlink_event_group(
                "commandReceived",
                ui_text!(en = "Command Received", zh = "命令下发"),
                4,
            ),
            build_downlink_event_group(
                "rpcResponseReceived",
                ui_text!(en = "RPC Response Received", zh = "RPC 响应"),
                5,
            ),
        ],
    })
}

fn build_downlink_event_group(group_key: &str, label: UiText, order: i32) -> Node {
    let downlink_enabled_path = "downlink.enabled";
    let enabled_path = format!("downlink.{group_key}.enabled");
    let topic_path = format!("downlink.{group_key}.topic");
    let payload_mode_path = format!("downlink.{group_key}.payload.mode");
    let payload_mapped_config_path = format!("downlink.{group_key}.payload.config");
    let payload_filter_mode_path = format!("downlink.{group_key}.payload.filter.mode");
    let payload_filter_pointer_path = format!("downlink.{group_key}.payload.filter.pointer");
    let payload_filter_key_path = format!("downlink.{group_key}.payload.filter.key");
    let payload_filter_equals_path = format!("downlink.{group_key}.payload.filter.equals");
    let ack_policy_path = format!("downlink.{group_key}.ackPolicy");
    let failure_policy_path = format!("downlink.{group_key}.failurePolicy");

    let placeholder_help = ui_text!(
        en = "Exact Kafka topic to subscribe. Templates/wildcards/regex are NOT supported. Put request_id in key/headers/payload, not in topic.",
        zh = "订阅的 Kafka 精确 Topic（Exact）。不支持模板/通配符/正则。request_id 请放在 key/headers/payload，不要放在 topic。"
    );

    Node::Group(Group {
        id: format!("downlink.{group_key}"),
        label,
        description: Some(ui_text!(
            en = "event_kind is fixed by the mapping slot.",
            zh = "event_kind 由映射槽位固定决定。"
        )),
        collapsible: true,
        order: Some(order),
        children: vec![
            Node::Field(Box::new(Field {
                path: enabled_path.clone(),
                label: ui_text!(en = "Enabled", zh = "启用"),
                data_type: UiDataType::Boolean,
                default_value: Some(json!(false)),
                order: Some(1),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: topic_path.clone(),
                label: ui_text!(en = "Topic", zh = "Topic"),
                data_type: UiDataType::String,
                default_value: Some(json!("ng.downlink")),
                order: Some(2),
                ui: Some(UiProps {
                    help: Some(placeholder_help),
                    ..Default::default()
                }),
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(en = "Topic is required", zh = "Topic 必填")),
                    }),
                    min_length: Some(RuleValue::Value(1)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Require },
                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: payload_mode_path.clone(),
                label: ui_text!(en = "Payload Mode", zh = "Payload 模式"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem { key: json!("envelope_json"), label: ui_text!(en = "envelope_json", zh = "envelope_json") },
                        EnumItem { key: json!("mapped_json"), label: ui_text!(en = "mapped_json", zh = "mapped_json") },
                    ],
                },
                default_value: Some(json!("envelope_json")),
                order: Some(3),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "How to decode the incoming Kafka JSON into a gateway downlink event.",
                        zh = "如何将 Kafka 下行 JSON 解码为网关下行事件。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules { required: Some(RuleValue::Value(true)), ..Default::default() }),
                when: Some(vec![
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Union(Union {
                order: Some(4),
                discriminator: payload_mode_path.clone(),
                mapping: vec![
                    UnionCase { case_value: json!("envelope_json"), children: vec![] },
                    UnionCase {
                        case_value: json!("mapped_json"),
                        children: vec![
                            Node::Field(Box::new(Field {
                                path: payload_mapped_config_path.clone(),
                                label: ui_text!(en = "Mapped Fields", zh = "映射字段"),
                                data_type: UiDataType::Any,
                                default_value: Some(json!({})),
                                order: Some(1),
                                ui: Some(UiProps {
                                    help: Some(ui_text!(
                                        en = "JSON object mapping: out_path -> expression.",
                                        zh = "JSON 对象映射：out_path -> expression。"
                                    )),
                                    ..Default::default()
                                }),
                                rules: Some(Rules { required: Some(RuleValue::Value(true)), ..Default::default() }),
                                when: Some(vec![
                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                ]),
                            })),
                            Node::Field(Box::new(Field {
                                path: payload_filter_mode_path.clone(),
                                label: ui_text!(en = "Mapped Filter Mode", zh = "映射过滤模式"),
                                data_type: UiDataType::Enum {
                                    items: vec![
                                        EnumItem { key: json!("none"), label: ui_text!(en = "None", zh = "无") },
                                        EnumItem { key: json!("json_pointer"), label: ui_text!(en = "JSON Pointer", zh = "JSON Pointer") },
                                        EnumItem { key: json!("property"), label: ui_text!(en = "Header", zh = "Header") },
                                        EnumItem { key: json!("key"), label: ui_text!(en = "Key", zh = "Key") },
                                    ],
                                },
                                default_value: Some(json!("none")),
                                order: Some(2),
                                ui: Some(UiProps {
                                    help: Some(ui_text!(
                                        en = "Optional discriminator for mapped_json to avoid noisy mapping errors when multiple routes share one topic.",
                                        zh = "mapped_json 的可选判别器：当多个事件共用一个 topic 时，避免把其它事件映射失败当成错误。"
                                    )),
                                    ..Default::default()
                                }),
                                rules: Some(Rules { required: Some(RuleValue::Value(true)), ..Default::default() }),
                                when: Some(vec![
                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                ]),
                            })),
                            Node::Union(Union {
                                order: Some(3),
                                discriminator: payload_filter_mode_path.clone(),
                                mapping: vec![
                                    UnionCase { case_value: json!("none"), children: vec![] },
                                    UnionCase {
                                        case_value: json!("json_pointer"),
                                        children: vec![
                                            Node::Field(Box::new(Field {
                                                path: payload_filter_pointer_path.clone(),
                                                label: ui_text!(en = "Pointer", zh = "Pointer"),
                                                data_type: UiDataType::String,
                                                default_value: Some(json!("/event_type")),
                                                order: Some(1),
                                                ui: Some(UiProps {
                                                    help: Some(ui_text!(
                                                        en = "JSON Pointer (RFC 6901), e.g. /event_type or /event/kind",
                                                        zh = "JSON Pointer（RFC 6901），例如 /event_type 或 /event/kind"
                                                    )),
                                                    ..Default::default()
                                                }),
                                                rules: Some(Rules { required: Some(RuleValue::Value(true)), min_length: Some(RuleValue::Value(1)), ..Default::default() }),
                                                when: None,
                                            })),
                                            Node::Field(Box::new(Field {
                                                path: payload_filter_equals_path.clone(),
                                                label: ui_text!(en = "Equals", zh = "等于"),
                                                data_type: UiDataType::String,
                                                default_value: None,
                                                order: Some(2),
                                                ui: None,
                                                rules: Some(Rules { required: Some(RuleValue::Value(true)), min_length: Some(RuleValue::Value(1)), ..Default::default() }),
                                                when: None,
                                            })),
                                        ],
                                    },
                                    UnionCase {
                                        case_value: json!("property"),
                                        children: vec![
                                            Node::Field(Box::new(Field {
                                                path: payload_filter_key_path.clone(),
                                                label: ui_text!(en = "Header Key", zh = "Header Key"),
                                                data_type: UiDataType::String,
                                                default_value: Some(json!("event_type")),
                                                order: Some(1),
                                                ui: None,
                                                rules: Some(Rules { required: Some(RuleValue::Value(true)), min_length: Some(RuleValue::Value(1)), ..Default::default() }),
                                                when: None,
                                            })),
                                            Node::Field(Box::new(Field {
                                                path: payload_filter_equals_path.clone(),
                                                label: ui_text!(en = "Equals", zh = "等于"),
                                                data_type: UiDataType::String,
                                                default_value: None,
                                                order: Some(2),
                                                ui: None,
                                                rules: Some(Rules { required: Some(RuleValue::Value(true)), min_length: Some(RuleValue::Value(1)), ..Default::default() }),
                                                when: None,
                                            })),
                                        ],
                                    },
                                    UnionCase {
                                        case_value: json!("key"),
                                        children: vec![Node::Field(Box::new(Field {
                                            path: payload_filter_equals_path.clone(),
                                            label: ui_text!(en = "Equals", zh = "等于"),
                                            data_type: UiDataType::String,
                                            default_value: None,
                                            order: Some(1),
                                            ui: Some(UiProps {
                                                help: Some(ui_text!(
                                                    en = "Compare Kafka record key (UTF-8).",
                                                    zh = "比较 Kafka record key（UTF-8）。"
                                                )),
                                                ..Default::default()
                                            }),
                                            rules: Some(Rules { required: Some(RuleValue::Value(true)), min_length: Some(RuleValue::Value(1)), ..Default::default() }),
                                            when: None,
                                        }))],
                                    },
                                ],
                            }),
                        ],
                    },
                ],
            }),
            Node::Field(Box::new(Field {
                path: ack_policy_path.clone(),
                label: ui_text!(en = "Ack Policy", zh = "Ack 策略"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem { key: json!("on_success"), label: ui_text!(en = "On Success", zh = "成功才提交") },
                        EnumItem { key: json!("always"), label: ui_text!(en = "Always", zh = "总是提交") },
                        EnumItem { key: json!("never"), label: ui_text!(en = "Never", zh = "从不提交") },
                    ],
                },
                default_value: Some(json!("on_success")),
                order: Some(5),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Controls when to commit offsets after handling (forwarded or ignored).",
                        zh = "控制下行消息处理后（已转发或被忽略）何时提交 offset。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules { required: Some(RuleValue::Value(true)), ..Default::default() }),
                when: Some(vec![
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: failure_policy_path.clone(),
                label: ui_text!(en = "Failure Policy", zh = "失败策略"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem { key: json!("drop"), label: ui_text!(en = "Drop", zh = "丢弃") },
                        EnumItem { key: json!("error"), label: ui_text!(en = "Error", zh = "报错") },
                    ],
                },
                default_value: Some(json!("drop")),
                order: Some(6),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "How to handle decode/forward failures.",
                        zh = "解码/转发失败时的处理方式。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules { required: Some(RuleValue::Value(true)), ..Default::default() }),
                when: Some(vec![
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                ]),
            })),
        ],
    })
}
