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
                    path: "bind_addr".into(),
                    label: ui_text!(en = "Bind Address", zh = "监听地址"),
                    data_type: UiDataType::String,
                    default_value: Some(json!("0.0.0.0:4840")),
                    order: Some(1),
                    ui: Some(UiProps {
                        help: Some(ui_text!(
                            en = "Local TCP socket bind address in 'host:port' form. Wildcards \
                                  '0.0.0.0' / '[::]' are allowed for multi-interface listening \
                                  (bare-metal multi-NIC, Docker bridge container internal). The \
                                  client-facing endpoint URLs are configured separately via \
                                  'Advertised Endpoints'.",
                            zh = "本地 TCP 套接字绑定地址，格式 host:port。允许使用通配符 '0.0.0.0' / '[::]' \
                                  以多接口监听（裸机多网卡 / Docker bridge 容器内部）。客户端可达的 \
                                  endpoint URL 由 '公告 Endpoint' 字段独立配置。"
                        )),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        // host:port ; host can be IPv4 / IPv6 (bracketed) / hostname / wildcard 0.0.0.0 / [::]
                        pattern: Some(RuleValue::WithMessage {
                            value: "^(?:\\[[0-9A-Fa-f:]+\\]|[A-Za-z0-9._-]+):(?:[1-9][0-9]{0,4})$".to_string(),
                            message: Some(ui_text!(
                                en = "Bind address must be 'host:port' (e.g. 0.0.0.0:4840 or [::]:4840 or 192.168.1.10:4840)",
                                zh = "绑定地址必须为 host:port 格式（如 0.0.0.0:4840 / [::]:4840 / 192.168.1.10:4840）"
                            )),
                        }),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "advertised_endpoints".into(),
                    label: ui_text!(en = "Advertised Endpoints", zh = "公告 Endpoint"),
                    data_type: UiDataType::Any,
                    default_value: Some(json!([])),
                    order: Some(2),
                    ui: Some(UiProps {
                        help: Some(ui_text!(
                            en = "REQUIRED non-empty list of OPC UA endpoint URLs the server publishes \
                                  to clients via discovery. Each entry MUST be a valid \
                                  'opc.tcp://host[:port][/path]' with a concrete host (no wildcards). \
                                  Strict OPC UA clients (KEPServerEX, UaExpert) reject any endpoint \
                                  whose host is 0.0.0.0 / [::]. Typical filling per deployment:\n\
                                  - bare-metal: [\"opc.tcp://192.168.1.10:4840/\", \"opc.tcp://gateway.local:4840/\"]\n\
                                  - Docker --network host: same as bare-metal\n\
                                  - Docker bridge '-p 4840:4840': [\"opc.tcp://<host_ip>:4840/\"]\n\
                                  - K8s NodePort 30840: [\"opc.tcp://<node_ip>:30840/\"]",
                            zh = "OPC UA 公告 endpoint URL 列表（必填非空），通过 GetEndpoints 暴露给客户端。\
                                  每项必须是合法的 'opc.tcp://host[:port][/path]'，host 必须是具体的主机名 \
                                  / IP，不允许 0.0.0.0 / [::]。严格的 OPC UA 客户端（KEPServerEX、UaExpert）\
                                  会拒绝公告了通配符地址的服务端。各部署形态推荐填法：\n\
                                  - 裸机：[\"opc.tcp://192.168.1.10:4840/\", \"opc.tcp://gateway.local:4840/\"]\n\
                                  - Docker --network host：同裸机\n\
                                  - Docker bridge '-p 4840:4840'：[\"opc.tcp://<宿主机 IP>:4840/\"]\n\
                                  - K8s NodePort 30840：[\"opc.tcp://<Node IP>:30840/\"]"
                        )),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "namespace_uri".into(),
                    label: ui_text!(en = "Namespace URI", zh = "命名空间 URI"),
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
                    label: ui_text!(en = "Application URI", zh = "应用 URI"),
                    data_type: UiDataType::String,
                    // Keep distinct from namespace_uri to avoid collisions with diagnostics namespace.
                    default_value: Some(json!("urn:ng:opcua-server")),
                    order: Some(4),
                    ui: Some(UiProps {
                        help: Some(ui_text!(
                            en = "OPC UA Application URI. MUST stay distinct from Namespace URI. \
                                  Changing this value triggers automatic certificate regeneration on \
                                  next start (the old certificate is archived).",
                            zh = "OPC UA Application URI，必须与 Namespace URI 不同。修改此值后下次启动 \
                                  会自动重新生成证书（旧证书归档保留）。"
                        )),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        required: Some(RuleValue::Value(true)),
                        ..Default::default()
                    }),
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "product_uri".into(),
                    label: ui_text!(en = "Product URI", zh = "产品 URI"),
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
                            en = "Optional. JSON array of trusted client application instance \
                                  certificates. Each item can be PEM (BEGIN/END CERTIFICATE) or \
                                  base64 DER. Materialized into the plugin PKI trust store \
                                  (trusted/) on startup.",
                            zh = "可选。受信客户端「应用实例证书」列表（JSON 数组）。每项可填 PEM \
                                  （含 BEGIN/END CERTIFICATE）或 base64 编码的 DER。插件启动时写入 \
                                  PKI 信任库 trusted/ 目录。"
                        )),
                        ..Default::default()
                    }),
                    rules: None,
                    when: None,
                })),
                Node::Field(Box::new(Field {
                    path: "cert_expiry_warn_days".into(),
                    label: ui_text!(en = "Certificate Expiry Warn (days)", zh = "证书到期告警阈值（天）"),
                    data_type: UiDataType::Integer,
                    default_value: Some(json!(30)),
                    order: Some(7),
                    ui: Some(UiProps {
                        help: Some(ui_text!(
                            en = "Days-to-expiry threshold below which the certificate-expiry \
                                  monitor emits a Warning. Below 3 days it escalates to Critical \
                                  and (if certificate self-management is enabled) auto-regenerates.",
                            zh = "证书剩余有效期 ≤ 此天数时发出 Warning 日志；剩余 ≤ 3 天时升级为 Critical \
                                  并触发自动续签（生成新证书并归档旧证书）。"
                        )),
                        ..Default::default()
                    }),
                    rules: Some(Rules {
                        min: Some(RuleValue::Value(1.0)),
                        max: Some(RuleValue::Value(365.0)),
                        ..Default::default()
                    }),
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
