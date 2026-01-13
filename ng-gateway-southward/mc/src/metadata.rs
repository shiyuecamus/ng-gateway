use ng_gateway_sdk::{ui_text, DriverSchemas, EnumItem, Field, Node, RuleValue, Rules, UiDataType};
use serde_json::json;

/// Build static metadata once to be embedded as JSON for the gateway UI/config.
///
/// This schema defines the editable `driver_config` fields for MC channels,
/// points and actions in the web console.
pub(super) fn build_metadata() -> DriverSchemas {
    DriverSchemas {
        channel: build_channel_nodes(),
        device: build_device_nodes(),
        point: build_point_nodes(),
        action: build_action_nodes(),
    }
}

/// Build channel-level configuration nodes for the MC driver.
fn build_channel_nodes() -> Vec<Node> {
    vec![
        // Host
        Node::Field(Box::new(Field {
            path: "host".into(),
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
        // Port
        Node::Field(Box::new(Field {
            path: "port".into(),
            label: ui_text!(en = "Port", zh = "端口"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(5001)),
            order: Some(2),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Port is required", zh = "端口是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: Some(ui_text!(en = "Port must be > 0", zh = "端口必须大于0")),
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
        // PLC Series
        Node::Field(Box::new(Field {
            path: "series".into(),
            label: ui_text!(en = "PLC Series", zh = "PLC 系列"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!("a"),
                        label: ui_text!(en = "A", zh = "A 系列"),
                    },
                    EnumItem {
                        key: json!("qnA"),
                        label: ui_text!(en = "QnA", zh = "QnA 系列"),
                    },
                    EnumItem {
                        key: json!("qL"),
                        label: ui_text!(en = "Q/L", zh = "Q/L 系列"),
                    },
                    EnumItem {
                        key: json!("iQR"),
                        label: ui_text!(en = "iQ-R", zh = "iQ-R 系列"),
                    },
                ],
            },
            default_value: Some(json!("qnA")),
            order: Some(3),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "PLC series is required",
                        zh = "PLC 系列是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        // Planner tuning (optional)
        Node::Field(Box::new(Field {
            path: "maxPointsPerBatch".into(),
            label: ui_text!(en = "Max Points Per Batch", zh = "单批最大点数"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(480)),
            order: Some(4),
            ui: None,
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: Some(ui_text!(en = "Min 1", zh = "最小为 1")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "maxBytesPerFrame".into(),
            label: ui_text!(en = "Max Bytes Per Frame", zh = "单帧最大字节数"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(4096)),
            order: Some(5),
            ui: None,
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: Some(ui_text!(en = "Min 1", zh = "最小为 1")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "concurrentRequests".into(),
            label: ui_text!(en = "Concurrent Requests", zh = "并发请求数"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(1)),
            order: Some(6),
            ui: None,
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 1.0,
                    message: Some(ui_text!(en = "Min 1", zh = "最小为 1")),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 64.0,
                    message: Some(ui_text!(en = "Max 64", zh = "最大为 64")),
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}

/// Build device-level configuration nodes for the MC driver.
fn build_device_nodes() -> Vec<Node> {
    vec![]
}

/// Build point-level configuration nodes for the MC driver.
fn build_point_nodes() -> Vec<Node> {
    vec![
        // MC logical address
        Node::Field(Box::new(Field {
            path: "address".into(),
            label: ui_text!(en = "MC Address", zh = "MC 地址"),
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
        })),
        // Optional string length in bytes for string data points
        Node::Field(Box::new(Field {
            path: "stringLenBytes".into(),
            label: ui_text!(en = "String Length (bytes)", zh = "字符串长度(字节)"),
            data_type: UiDataType::Integer,
            default_value: None,
            order: Some(2),
            ui: None,
            rules: None,
            when: None,
        })),
    ]
}

/// Build action-level configuration nodes for the MC driver.
fn build_action_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "address".into(),
        label: ui_text!(en = "MC Address", zh = "MC 地址"),
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
