use ng_gateway_sdk::{
    ui_text, EnumItem, Field, Group, Node, Operator, PluginConfigSchemas, RuleValue, Rules,
    UiDataType, UiProps, UiText, Union, UnionCase, When, WhenEffect,
};
use serde_json::json;

/// Build static metadata once to be embedded as JSON for the gateway UI/config.
///
/// Phase 1 focuses on uplink mapping. Phase 2 adds downlink subscription/mapping.
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
                path: "connection.serviceUrl".into(),
                label: ui_text!(en = "Service URL", zh = "服务地址"),
                data_type: UiDataType::String,
                default_value: Some(json!("pulsar://127.0.0.1:6650")),
                order: Some(1),
                ui: Some(UiProps {
                    placeholder: Some(ui_text!(
                        en = "pulsar://host:6650 or pulsar+ssl://host:6651",
                        zh = "pulsar://host:6650 或 pulsar+ssl://host:6651"
                    )),
                    help: Some(ui_text!(
                        en = "Use pulsar+ssl for TLS endpoints.",
                        zh = "TLS 端点请使用 pulsar+ssl。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules {
                    required: Some(RuleValue::WithMessage {
                        value: true,
                        message: Some(ui_text!(
                            en = "Service URL is required",
                            zh = "服务地址必填"
                        )),
                    }),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Field(Box::new(Field {
                path: "connection.auth.mode".into(),
                label: ui_text!(en = "Auth Mode", zh = "认证模式"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!("none"),
                            label: ui_text!(en = "None", zh = "无"),
                        },
                        EnumItem {
                            key: json!("token"),
                            label: ui_text!(en = "Token", zh = "Token"),
                        },
                    ],
                },
                default_value: Some(json!("none")),
                order: Some(2),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: None,
            })),
            Node::Union(Union {
                order: Some(3),
                discriminator: "connection.auth.mode".into(),
                mapping: vec![
                    UnionCase {
                        case_value: json!("none"),
                        children: vec![],
                    },
                    UnionCase {
                        case_value: json!("token"),
                        children: vec![Node::Field(Box::new(Field {
                            path: "connection.auth.token".into(),
                            label: ui_text!(en = "Token", zh = "Token"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(1),
                            ui: Some(UiProps {
                                placeholder: Some(ui_text!(
                                    en = "JWT / token value",
                                    zh = "JWT / token 值"
                                )),
                                ..Default::default()
                            }),
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: Some(ui_text!(
                                        en = "Token is required",
                                        zh = "Token 必填"
                                    )),
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        }))],
                    },
                ],
            }),
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
                path: "uplink.producer.compression".into(),
                label: ui_text!(en = "Compression", zh = "压缩"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!("none"),
                            label: ui_text!(en = "None", zh = "无"),
                        },
                        EnumItem {
                            key: json!("lz4"),
                            label: ui_text!(en = "LZ4", zh = "LZ4"),
                        },
                        EnumItem {
                            key: json!("zlib"),
                            label: ui_text!(en = "Zlib", zh = "Zlib"),
                        },
                        EnumItem {
                            key: json!("zstd"),
                            label: ui_text!(en = "Zstd", zh = "Zstd"),
                        },
                        EnumItem {
                            key: json!("snappy"),
                            label: ui_text!(en = "Snappy", zh = "Snappy"),
                        },
                    ],
                },
                default_value: Some(json!("lz4")),
                order: Some(1),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.batchingEnabled".into(),
                label: ui_text!(en = "Batching Enabled", zh = "启用批量发送"),
                data_type: UiDataType::Boolean,
                default_value: Some(json!(false)),
                order: Some(2),
                ui: None,
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.batchingMaxMessages".into(),
                label: ui_text!(en = "Batch Max Messages", zh = "批量最大消息数"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(1000)),
                order: Some(3),
                ui: None,
                rules: Some(Rules {
                    min: Some(RuleValue::Value(1.0)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: "uplink.producer.batchingEnabled".into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.batchingMaxBytes".into(),
                label: ui_text!(en = "Batch Max Bytes", zh = "批量最大字节数"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(131072)),
                order: Some(4),
                ui: None,
                rules: Some(Rules {
                    min: Some(RuleValue::Value(1.0)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: "uplink.producer.batchingEnabled".into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: "uplink.producer.batchingMaxPublishDelayMs".into(),
                label: ui_text!(en = "Batch Max Delay (ms)", zh = "批量最大延迟(ms)"),
                data_type: UiDataType::Integer,
                default_value: Some(json!(10)),
                order: Some(5),
                ui: None,
                rules: Some(Rules {
                    min: Some(RuleValue::Value(1.0)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: "uplink.producer.batchingEnabled".into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: "uplink.enabled".into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
        ],
    })
}

fn build_uplink_group() -> Node {
    Node::Group(Group {
        id: "uplink".into(),
        label: ui_text!(en = "Uplink", zh = "上行"),
        description: Some(ui_text!(
            en = "Gateway -> Pulsar mappings by event kind.",
            zh = "按事件类型配置 Gateway -> Pulsar 映射。"
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
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Master switch for all uplink mappings.",
                        zh = "上行总开关（控制所有上行映射）。"
                    )),
                    ..Default::default()
                }),
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

fn build_downlink_group() -> Node {
    Node::Group(Group {
        id: "downlink".into(),
        label: ui_text!(en = "Downlink", zh = "下行"),
        description: Some(ui_text!(
            en = "Pulsar -> Gateway. Enable downlink to start subscription.",
            zh = "Pulsar -> Gateway：启用下行后才会启动订阅。"
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
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Master switch for downlink subscription and mappings.",
                        zh = "下行总开关（控制订阅与所有映射）。"
                    )),
                    ..Default::default()
                }),
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
        en = "Exact Pulsar topic to subscribe. Templates/wildcards/regex are NOT supported. Put request_id in key/properties/payload, not in topic.",
        zh = "订阅的 Pulsar 精确 Topic（Exact）。不支持模板/通配符/正则。request_id 请放在 key/properties/payload，不要放在 topic。"
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
                    When {
                        target: downlink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(true),
                        effect: WhenEffect::Visible,
                    },
                    When {
                        target: downlink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: topic_path.clone(),
                label: ui_text!(en = "Topic", zh = "Topic"),
                data_type: UiDataType::String,
                default_value: Some(json!("persistent://public/default/ng.downlink")),
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
                    When {
                        target: downlink_enabled_path.into(),
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
                        target: downlink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: payload_mode_path.clone(),
                label: ui_text!(en = "Payload Mode", zh = "Payload 模式"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!("envelope_json"),
                            label: ui_text!(
                                en = "envelope_json",
                                zh = "envelope_json"
                            ),
                        },
                        EnumItem {
                            key: json!("mapped_json"),
                            label: ui_text!(en = "mapped_json", zh = "mapped_json"),
                        },
                    ],
                },
                default_value: Some(json!("envelope_json")),
                order: Some(3),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "How to decode the incoming Pulsar JSON into a gateway downlink event.",
                        zh = "如何将 Pulsar 下行 JSON 解码为网关下行事件。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: downlink_enabled_path.into(),
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
                        target: downlink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Union(Union {
                order: Some(4),
                discriminator: payload_mode_path.clone(),
                mapping: vec![
                    UnionCase {
                        case_value: json!("envelope_json"),
                        children: vec![],
                    },
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
                                        en = "JSON object mapping: out_path -> expression. Example: {\"payload.data.point_id\":\"$.payload.data.pointId\"}",
                                        zh = "JSON 对象映射：out_path -> expression。示例：{\"payload.data.point_id\":\"$.payload.data.pointId\"}"
                                    )),
                                    ..Default::default()
                                }),
                                rules: Some(Rules {
                                    required: Some(RuleValue::Value(true)),
                                    ..Default::default()
                                }),
                                // NOTE: `Union` itself has no `when`. Gate visibility by `enabled` to avoid
                                // showing mapping fields for disabled events.
                                when: Some(vec![
                                    When {
                                        target: downlink_enabled_path.into(),
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
                                        target: downlink_enabled_path.into(),
                                        operator: Operator::Eq,
                                        value: json!(false),
                                        effect: WhenEffect::Invisible,
                                    },
                                ]),
                            })),
                            Node::Field(Box::new(Field {
                                path: payload_filter_mode_path.clone(),
                                label: ui_text!(en = "Mapped Filter Mode", zh = "映射过滤模式"),
                                data_type: UiDataType::Enum {
                                    items: vec![
                                        EnumItem { key: json!("none"), label: ui_text!(en = "None", zh = "无") },
                                        EnumItem { key: json!("json_pointer"), label: ui_text!(en = "JSON Pointer", zh = "JSON Pointer") },
                                        EnumItem { key: json!("pulsar_property"), label: ui_text!(en = "Pulsar Property", zh = "Pulsar 属性") },
                                        EnumItem { key: json!("pulsar_key"), label: ui_text!(en = "Pulsar Key", zh = "Pulsar Key") },
                                    ],
                                },
                                default_value: Some(json!("none")),
                                order: Some(2),
                                ui: Some(UiProps {
                                    help: Some(ui_text!(
                                        en = "Optional discriminator for mapped_json to avoid mapping errors when multiple routes share one topic.",
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
                                                when: Some(vec![
                                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                                ]),
                                            })),
                                            Node::Field(Box::new(Field {
                                                path: payload_filter_equals_path.clone(),
                                                label: ui_text!(en = "Equals", zh = "等于"),
                                                data_type: UiDataType::String,
                                                default_value: None,
                                                order: Some(2),
                                                ui: None,
                                                rules: Some(Rules { required: Some(RuleValue::Value(true)), min_length: Some(RuleValue::Value(1)), ..Default::default() }),
                                                when: Some(vec![
                                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                                ]),
                                            })),
                                        ],
                                    },
                                    UnionCase {
                                        case_value: json!("pulsar_property"),
                                        children: vec![
                                            Node::Field(Box::new(Field {
                                                path: payload_filter_key_path.clone(),
                                                label: ui_text!(en = "Property Key", zh = "属性 Key"),
                                                data_type: UiDataType::String,
                                                default_value: Some(json!("event_type")),
                                                order: Some(1),
                                                ui: None,
                                                rules: Some(Rules { required: Some(RuleValue::Value(true)), min_length: Some(RuleValue::Value(1)), ..Default::default() }),
                                                when: Some(vec![
                                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                                ]),
                                            })),
                                            Node::Field(Box::new(Field {
                                                path: payload_filter_equals_path.clone(),
                                                label: ui_text!(en = "Equals", zh = "等于"),
                                                data_type: UiDataType::String,
                                                default_value: None,
                                                order: Some(2),
                                                ui: None,
                                                rules: Some(Rules { required: Some(RuleValue::Value(true)), min_length: Some(RuleValue::Value(1)), ..Default::default() }),
                                                when: Some(vec![
                                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                    When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                                    When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                                ]),
                                            })),
                                        ],
                                    },
                                    UnionCase {
                                        case_value: json!("pulsar_key"),
                                        children: vec![Node::Field(Box::new(Field {
                                            path: payload_filter_equals_path.clone(),
                                            label: ui_text!(en = "Equals", zh = "等于"),
                                            data_type: UiDataType::String,
                                            default_value: None,
                                            order: Some(1),
                                            ui: Some(UiProps {
                                                help: Some(ui_text!(
                                                    en = "Compare Pulsar message key (partition_key).",
                                                    zh = "比较 Pulsar 消息 key（partition_key）。"
                                                )),
                                                ..Default::default()
                                            }),
                                            rules: Some(Rules { required: Some(RuleValue::Value(true)), min_length: Some(RuleValue::Value(1)), ..Default::default() }),
                                            when: Some(vec![
                                                When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(true), effect: WhenEffect::Visible },
                                                When { target: enabled_path.clone(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                                When { target: downlink_enabled_path.into(), operator: Operator::Eq, value: json!(false), effect: WhenEffect::Invisible },
                                            ]),
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
                        EnumItem {
                            key: json!("on_success"),
                            label: ui_text!(en = "On Success", zh = "成功才 Ack"),
                        },
                        EnumItem {
                            key: json!("always"),
                            label: ui_text!(en = "Always", zh = "总是 Ack"),
                        },
                        EnumItem {
                            key: json!("never"),
                            label: ui_text!(en = "Never", zh = "从不 Ack"),
                        },
                    ],
                },
                default_value: Some(json!("on_success")),
                order: Some(5),
                ui: Some(UiProps {
                    help: Some(ui_text!(
                        en = "Controls when to ack Pulsar messages after handling (forwarded or ignored).",
                        zh = "控制下行消息处理后（已转发或被忽略）何时 Ack。"
                    )),
                    ..Default::default()
                }),
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: downlink_enabled_path.into(),
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
                        target: downlink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
                ]),
            })),
            Node::Field(Box::new(Field {
                path: failure_policy_path.clone(),
                label: ui_text!(en = "Failure Policy", zh = "失败策略"),
                data_type: UiDataType::Enum {
                    items: vec![
                        EnumItem {
                            key: json!("drop"),
                            label: ui_text!(en = "Drop", zh = "丢弃"),
                        },
                        EnumItem {
                            key: json!("error"),
                            label: ui_text!(en = "Error", zh = "报错"),
                        },
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
                rules: Some(Rules {
                    required: Some(RuleValue::Value(true)),
                    ..Default::default()
                }),
                when: Some(vec![
                    When {
                        target: downlink_enabled_path.into(),
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
                        target: downlink_enabled_path.into(),
                        operator: Operator::Eq,
                        value: json!(false),
                        effect: WhenEffect::Invisible,
                    },
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

    // Only telemetry/attributes support kv / timeseries_rows payloads.
    // For deviceConnected/deviceDisconnected, keep payload modes minimal.
    let allow_kv_ts_rows = matches!(group_key, "telemetry" | "attributes");

    let placeholder_help = ui_text!(
        en = "Handlebars vars: {{app_id}} {{app_name}} {{plugin_type}} {{event_kind}} {{ts_ms}} {{device_id}} {{device_name}} {{device_type}} {{channel_name}}",
        zh = "Handlebars 变量：{{app_id}} {{app_name}} {{plugin_type}} {{event_kind}} {{ts_ms}} {{device_id}} {{device_name}} {{device_type}} {{channel_name}}"
    );

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
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    // NOTE: `Union` itself has no `when`. Gate visibility by `enabled` to avoid
                    // showing payload fields for disabled events.
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
                    en = "JSON object mapping: out_path -> expression. Example: {\"payload.data.device_id\":\"$.device_id\"}",
                    zh = "JSON 对象映射：out_path -> expression。示例：{\"payload.data.device_id\":\"$.device_id\"}"
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
                default_value: Some(json!(
                    "persistent://public/default/ng.uplink.{{event_kind}}.{{device_name}}"
                )),
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
                        en = "Used as Pulsar partition_key. Default keeps per-device ordering.",
                        zh = "作为 Pulsar partition_key。默认保证同设备有序。"
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
