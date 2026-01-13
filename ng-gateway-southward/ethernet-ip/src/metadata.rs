use ng_gateway_sdk::{ui_text, DriverSchemas, Field, Node, RuleValue, Rules, UiDataType};
use serde_json::json;

/// Build static metadata once to be embedded as JSON for the gateway UI/config.
pub(super) fn build_metadata() -> DriverSchemas {
    DriverSchemas {
        channel: build_channel_nodes(),
        device: build_device_nodes(),
        point: build_point_nodes(),
        action: build_action_nodes(),
    }
}

/// Build channel-level configuration nodes for the Ethernet/IP driver.
fn build_channel_nodes() -> Vec<Node> {
    vec![
        Node::Field(Box::new(Field {
            path: "host".into(),
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
                // Hostname (RFC-1123 labels) or IPv4
                pattern: Some(RuleValue::WithMessage {
                    value: "^(?:(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)(?:\\.(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?))*|(?:(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)\\.){3}(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d))$".to_string(),
                    message: Some(ui_text!(
                        en = "Enter a valid IPv4 address or hostname",
                        zh = "请输入有效的 IPv4 或主机名"
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
            path: "port".into(),
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
            default_value: Some(json!(44818)),
            order: Some(2),
            ui: None,
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "timeout".into(),
            label: ui_text!(en = "Timeout (ms)", zh = "超时时间 (ms)"),
            data_type: UiDataType::Integer,
            rules: Some(Rules {
                min: Some(RuleValue::WithMessage {
                    value: 100.0,
                    message: Some(ui_text!(
                        en = "Timeout must be at least 100ms",
                        zh = "超时时间至少为 100ms"
                    )),
                }),
                ..Default::default()
            }),
            default_value: Some(json!(2000)),
            order: Some(3),
            ui: None,
            when: None,
        })),
        Node::Field(Box::new(Field {
            path: "slot".into(),
            label: ui_text!(en = "Slot", zh = "插槽号"),
            data_type: UiDataType::Integer,
            default_value: Some(json!(0)),
            order: Some(4),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(en = "Slot is required", zh = "插槽号是必填项")),
                }),
                min: Some(RuleValue::WithMessage {
                    value: 0.0,
                    message: Some(ui_text!(
                        en = "Slot must be non-negative",
                        zh = "插槽号必须是非负数"
                    )),
                }),
                max: Some(RuleValue::WithMessage {
                    value: 255.0,
                    message: Some(ui_text!(
                        en = "Slot must be less than 256",
                        zh = "插槽号必须小于256"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}

/// Build device-level configuration nodes for the Ethernet/IP driver.
fn build_device_nodes() -> Vec<Node> {
    vec![]
}

/// Build point-level configuration nodes for the Ethernet/IP driver.
fn build_point_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "tagName".into(),
        label: ui_text!(en = "Tag Name", zh = "标签名称"),
        data_type: UiDataType::String,
        default_value: None,
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: Some(ui_text!(
                    en = "Tag Name is required",
                    zh = "标签名称是必填项"
                )),
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}

/// Build action-level configuration nodes for the Ethernet/IP driver.
fn build_action_nodes() -> Vec<Node> {
    // Actions typically reuse point configuration or define specific commands.
    // For basic tag writing, we might just need the tag name.
    vec![Node::Field(Box::new(Field {
        path: "tagName".into(),
        label: ui_text!(en = "Tag Name", zh = "标签名称"),
        data_type: UiDataType::String,
        default_value: None,
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: Some(ui_text!(
                    en = "Tag Name is required",
                    zh = "标签名称是必填项"
                )),
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}
