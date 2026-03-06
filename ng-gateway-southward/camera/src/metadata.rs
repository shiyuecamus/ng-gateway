//! Camera driver UiSchema metadata.
//!
//! Defines the UI configuration forms for channel, device, point, and action
//! entities. These schemas are serialized to JSON and consumed by the gateway
//! UI to render dynamic configuration forms for camera channels.

use crate::types::{CameraCommand, CameraOutputKey};
use ng_gateway_sdk::{
    ui_text, DriverSchemas, EnumItem, Field, Node, RuleValue, Rules, UiDataType, UiProps, Union,
    UnionCase,
};
use serde_json::json;

/// Build static metadata for the Camera driver UI schemas.
pub(crate) fn build_camera_schemas() -> DriverSchemas {
    DriverSchemas {
        channel: build_channel_nodes(),
        device: build_device_nodes(),
        point: build_point_nodes(),
        action: build_action_nodes(),
    }
}

// ─── Channel Schema ────────────────────────────────────────────────

fn build_channel_nodes() -> Vec<Node> {
    vec![
        // ── Protocol type selector ─────────────────────────────
        Node::Field(Box::new(Field {
            path: "protocol.type".into(),
            label: ui_text!(en = "Protocol Type", zh = "协议类型"),
            data_type: UiDataType::Enum {
                items: vec![
                    EnumItem {
                        key: json!("rtsp"),
                        label: ui_text!(en = "RTSP", zh = "RTSP"),
                    },
                    EnumItem {
                        key: json!("onvif"),
                        label: ui_text!(en = "ONVIF", zh = "ONVIF"),
                    },
                    EnumItem {
                        key: json!("mjpeg"),
                        label: ui_text!(en = "HTTP MJPEG", zh = "HTTP MJPEG"),
                    },
                ],
            },
            default_value: Some(json!("rtsp")),
            order: Some(1),
            ui: None,
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "Protocol type is required",
                        zh = "协议类型是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
        // ── Protocol-specific fields (Union) ───────────────────
        Node::Union(Union {
            order: Some(2),
            discriminator: "protocol.type".into(),
            mapping: vec![
                // ─── RTSP fields ───────────────────────────────
                UnionCase {
                    case_value: json!("rtsp"),
                    children: vec![
                        Node::Field(Box::new(Field {
                            path: "protocol.url".into(),
                            label: ui_text!(en = "RTSP URL", zh = "RTSP 地址"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(3),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: Some(ui_text!(
                                        en = "RTSP URL is required",
                                        zh = "RTSP 地址是必填项"
                                    )),
                                }),
                                pattern: Some(RuleValue::WithMessage {
                                    value: r"^rtsp://".to_string(),
                                    message: Some(ui_text!(
                                        en = "URL must start with rtsp://",
                                        zh = "地址必须以 rtsp:// 开头"
                                    )),
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "protocol.transport".into(),
                            label: ui_text!(en = "Transport", zh = "传输方式"),
                            data_type: UiDataType::Enum {
                                items: vec![
                                    EnumItem {
                                        key: json!("tcp"),
                                        label: ui_text!(en = "TCP (reliable)", zh = "TCP（可靠）"),
                                    },
                                    EnumItem {
                                        key: json!("udp"),
                                        label: ui_text!(
                                            en = "UDP (low latency)",
                                            zh = "UDP（低延迟）"
                                        ),
                                    },
                                ],
                            },
                            default_value: Some(json!("tcp")),
                            order: Some(4),
                            ui: None,
                            rules: None,
                            when: None,
                        })),
                    ],
                },
                // ─── ONVIF fields ──────────────────────────────
                UnionCase {
                    case_value: json!("onvif"),
                    children: vec![
                        Node::Field(Box::new(Field {
                            path: "protocol.endpoint".into(),
                            label: ui_text!(en = "ONVIF Endpoint", zh = "ONVIF 端点"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(3),
                            ui: None,
                            rules: Some(Rules {
                                required: Some(RuleValue::WithMessage {
                                    value: true,
                                    message: Some(ui_text!(
                                        en = "ONVIF endpoint is required",
                                        zh = "ONVIF 端点是必填项"
                                    )),
                                }),
                                ..Default::default()
                            }),
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "protocol.username".into(),
                            label: ui_text!(en = "Username", zh = "用户名"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(4),
                            ui: None,
                            rules: None,
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "protocol.password".into(),
                            label: ui_text!(en = "Password", zh = "密码"),
                            data_type: UiDataType::String,
                            default_value: None,
                            order: Some(5),
                            ui: None,
                            rules: None,
                            when: None,
                        })),
                        Node::Field(Box::new(Field {
                            path: "protocol.profile".into(),
                            label: ui_text!(en = "Media Profile", zh = "媒体配置"),
                            data_type: UiDataType::String,
                            default_value: Some(json!("")),
                            order: Some(6),
                            ui: None,
                            rules: None,
                            when: None,
                        })),
                    ],
                },
                // ─── MJPEG fields ──────────────────────────────
                UnionCase {
                    case_value: json!("mjpeg"),
                    children: vec![Node::Field(Box::new(Field {
                        path: "protocol.url".into(),
                        label: ui_text!(en = "MJPEG URL", zh = "MJPEG 地址"),
                        data_type: UiDataType::String,
                        default_value: None,
                        order: Some(3),
                        ui: None,
                        rules: Some(Rules {
                            required: Some(RuleValue::WithMessage {
                                value: true,
                                message: Some(ui_text!(
                                    en = "MJPEG URL is required",
                                    zh = "MJPEG 地址是必填项"
                                )),
                            }),
                            pattern: Some(RuleValue::WithMessage {
                                value: r"^https?://".to_string(),
                                message: Some(ui_text!(
                                    en = "URL must start with http:// or https://",
                                    zh = "地址必须以 http:// 或 https:// 开头"
                                )),
                            }),
                            ..Default::default()
                        }),
                        when: None,
                    }))],
                },
            ],
        }),
        // ── AI Pipeline Configuration ──────────────────────────
        Node::Field(Box::new(Field {
            path: "pipelineId".into(),
            label: ui_text!(en = "AI Pipeline", zh = "AI 分析流水线"),
            data_type: UiDataType::Integer,
            default_value: None,
            order: Some(10),
            ui: Some(UiProps::api_select_with_create(
                "/api/ai/pipelines/list",
                "id",
                "name",
                "/ai/pipeline",
            )),
            rules: Some(Rules {
                required: Some(RuleValue::WithMessage {
                    value: true,
                    message: Some(ui_text!(
                        en = "AI pipeline is required",
                        zh = "AI 分析流水线是必填项"
                    )),
                }),
                ..Default::default()
            }),
            when: None,
        })),
    ]
}

// ─── Device Schema ─────────────────────────────────────────────────
// Camera devices are lightweight (just identity). The channel carries
// all protocol and pipeline configuration.

fn build_device_nodes() -> Vec<Node> {
    vec![]
}

// ─── Point Schema ──────────────────────────────────────────────────
// Points map AI analysis outputs to northward data points.

fn build_point_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "outputKey".into(),
        label: ui_text!(en = "AI Output Type", zh = "AI 输出类型"),
        data_type: UiDataType::Enum {
            items: vec![
                EnumItem {
                    key: json!(CameraOutputKey::DetectionCount.as_str()),
                    label: ui_text!(en = "Detection Count", zh = "检测数量"),
                },
                EnumItem {
                    key: json!(CameraOutputKey::PersonCount.as_str()),
                    label: ui_text!(en = "Person Count", zh = "人数"),
                },
                EnumItem {
                    key: json!(CameraOutputKey::VehicleCount.as_str()),
                    label: ui_text!(en = "Vehicle Count", zh = "车辆数"),
                },
                EnumItem {
                    key: json!(CameraOutputKey::InferenceLatencyMs.as_str()),
                    label: ui_text!(en = "Inference Latency (ms)", zh = "推理延迟(ms)"),
                },
                EnumItem {
                    key: json!(CameraOutputKey::DetectionJson.as_str()),
                    label: ui_text!(en = "Detection JSON", zh = "检测结果JSON"),
                },
                EnumItem {
                    key: json!(CameraOutputKey::AlarmActive.as_str()),
                    label: ui_text!(en = "Alarm Active", zh = "告警激活"),
                },
                EnumItem {
                    key: json!(CameraOutputKey::TopClass.as_str()),
                    label: ui_text!(en = "Top Classification", zh = "最高分类"),
                },
                EnumItem {
                    key: json!(CameraOutputKey::TopConfidence.as_str()),
                    label: ui_text!(en = "Top Confidence", zh = "最高置信度"),
                },
                EnumItem {
                    key: json!(CameraOutputKey::Custom.as_str()),
                    label: ui_text!(en = "Custom Expression", zh = "自定义表达式"),
                },
            ],
        },
        default_value: Some(json!(CameraOutputKey::DetectionCount.as_str())),
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: Some(ui_text!(
                    en = "Output type is required",
                    zh = "输出类型是必填项"
                )),
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}

// ─── Action Schema ─────────────────────────────────────────────────
// Camera actions: PTZ control, snapshot capture, pipeline restart.

fn build_action_nodes() -> Vec<Node> {
    vec![Node::Field(Box::new(Field {
        path: "actionType".into(),
        label: ui_text!(en = "Action Type", zh = "动作类型"),
        data_type: UiDataType::Enum {
            items: vec![
                EnumItem {
                    key: json!(CameraCommand::PtzMove.as_str()),
                    label: ui_text!(en = "PTZ Move", zh = "云台控制"),
                },
                EnumItem {
                    key: json!(CameraCommand::PtzStop.as_str()),
                    label: ui_text!(en = "PTZ Stop", zh = "云台停止"),
                },
                EnumItem {
                    key: json!(CameraCommand::PtzPreset.as_str()),
                    label: ui_text!(en = "PTZ Go to Preset", zh = "云台预置位"),
                },
                EnumItem {
                    key: json!(CameraCommand::Snapshot.as_str()),
                    label: ui_text!(en = "Capture Snapshot", zh = "抓拍截图"),
                },
                EnumItem {
                    key: json!(CameraCommand::RestartPipeline.as_str()),
                    label: ui_text!(en = "Restart Pipeline", zh = "重启分析流水线"),
                },
            ],
        },
        default_value: Some(json!(CameraCommand::Snapshot.as_str())),
        order: Some(1),
        ui: None,
        rules: Some(Rules {
            required: Some(RuleValue::WithMessage {
                value: true,
                message: Some(ui_text!(
                    en = "Action type is required",
                    zh = "动作类型是必填项"
                )),
            }),
            ..Default::default()
        }),
        when: None,
    }))]
}
