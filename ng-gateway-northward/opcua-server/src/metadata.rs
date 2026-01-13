use ng_gateway_sdk::{
    ui_text, EnumItem, Field, Group, Node, PluginConfigSchemas, RuleValue, Rules, UiDataType,
    UiProps,
};
use serde_json::json;

/// Build static metadata once to be embedded as JSON for the gateway UI/config.
pub(super) fn build_metadata() -> PluginConfigSchemas {
    vec![
        Node::Group(Group {
            id: "server".into(),
            label: ui_text!(en = "Server", zh = "服务端"),
            description: None,
            collapsible: false,
            order: Some(1),
            children: vec![
                Node::Field(Box::new(Field {
                    path: "host".into(),
                    label: ui_text!(en = "Host", zh = "主机"),
                    data_type: UiDataType::String,
                    default_value: Some(json!("0.0.0.0")),
                    order: Some(1),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
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
                    path: "port".into(),
                    label: ui_text!(en = "Port", zh = "端口"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(4840)),
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
                Node::Field(Box::new(Field {
                    path: "namespace_uri".into(),
                    label: ui_text!(en = "Namespace URI", zh = "命名空间URI"),
                    data_type: UiDataType::String,
                    default_value: Some(json!("urn:ng:ng-gateway")),
                    order: Some(3),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "application_uri".into(),
                    label: ui_text!(en = "Application URI", zh = "应用URI"),
                    data_type: UiDataType::String,
                    // Keep distinct from namespace_uri to avoid collisions with diagnostics namespace.
                    default_value: Some(json!("urn:ng:opcua-server")),
                    order: Some(4),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "product_uri".into(),
                    label: ui_text!(en = "Product URI", zh = "产品URI"),
                    data_type: UiDataType::String,
                    default_value: Some(json!("urn:ng:opcua-server")),
                    order: Some(5),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "trusted_client_certs".into(),
                    label: ui_text!(en = "Trusted Client Certificates", zh = "受信客户端证书"),
                    data_type: UiDataType::Any,
                    default_value: Some(json!([])),
                    order: Some(6),
                    ui: Some(UiProps {
                        help: Some(ui_text!(
                            en = "Optional. JSON array of trusted client application instance certificates. Each item can be PEM (BEGIN/END CERTIFICATE) or base64 DER. These will be written into the plugin PKI trust store (trusted/) on startup.",
                            zh = "可选。受信客户端“应用实例证书”列表（JSON 数组）。每项可填写 PEM（含 BEGIN/END CERTIFICATE）或 base64 编码的 DER。插件启动时会写入 PKI 信任库 trusted/ 目录。"
                        )),
                        ..Default::default()
                    }),
                    rules: None,
                    when: None,
                })),
            ],
        }),
        Node::Group(Group {
            id: "updates".into(),
            label: ui_text!(en = "Updates", zh = "更新/背压"),
            description: None,
            collapsible: false,
            order: Some(2),
            children: vec![
                Node::Field(Box::new(Field {
                    path: "update_queue_capacity".into(),
                    label: ui_text!(en = "Queue Capacity", zh = "队列容量"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(10000)),
                    order: Some(1),
                    ui: None,
                    rules: Some(Rules {
                        min: Some(RuleValue::Value(1.0)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "drop_policy".into(),
                    label: ui_text!(en = "Drop Policy", zh = "丢弃策略"),
                    data_type: UiDataType::Enum {
                        items: vec![
                            EnumItem {
                                key: json!("discard_oldest"),
                                label: ui_text!(en = "Discard Oldest", zh = "丢最旧"),
                            },
                            EnumItem {
                                key: json!("discard_newest"),
                                label: ui_text!(en = "Discard Newest", zh = "丢最新"),
                            },
                            EnumItem {
                                key: json!("block_with_timeout"),
                                label: ui_text!(en = "Block With Timeout", zh = "阻塞(超时)"),
                            },
                        ],
                    },
                    default_value: Some(json!("discard_oldest")),
                    order: Some(2),
                    ui: None,
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "write_timeout_ms".into(),
                    label: ui_text!(en = "Write Timeout", zh = "写入超时"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(5000)),
                    order: Some(3),
                    ui: Some(UiProps {
                        help: Some(ui_text!(
                            en = "Overall timeout for a single OPC UA Write request (enqueue + southward write).",
                            zh = "单次 OPC UA 写入请求的整体超时（入队 + 南向写入）。"
                        )),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::Value(0.0)),
                        max: Some(RuleValue::Value(600_000.0)),
                        ..Default::default()
                    }),
                    when: None,
                })),
            ],
        }),
    ]
}
