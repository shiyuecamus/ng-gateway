use std::sync::Arc;

use ng_gateway_sdk::northward::downlink::{
    decode_event, AckPolicy, DownlinkKind, DownlinkMessageMeta, DownlinkPayloadConfig,
    DownlinkRoute, EventDownlink, FailurePolicy, MappedDownlinkFilterConfig,
};
use ng_gateway_sdk::northward::payload::{
    build_context, encode_uplink_payload, MappedJsonConfig, UplinkEventKind, UplinkPayloadConfig,
};
use ng_gateway_sdk::{
    AccessMode, DataType, NGValue, NorthwardData, NorthwardEvent, PointMeta, PointValue,
    TelemetryData,
};
use serde_json::{json, Value};

mod common;
use common::MockRuntime;

#[test]
fn test_uplink_mapped_json() {
    // 1. Setup Runtime & Data
    let runtime = Arc::new(MockRuntime::new().with_point(PointMeta {
        point_id: 101,
        channel_id: 1,
        channel_name: "ch1".into(),
        device_id: 1,
        device_name: "dev1".into(),
        point_name: "Temperature".into(),
        point_key: "temp".into(),
        data_type: DataType::Float64,
        access_mode: AccessMode::Read,
        unit: None,
        min_value: None,
        max_value: None,
        scale: None,
        description: None,
    }));

    let data = NorthwardData::Telemetry(TelemetryData::new(
        1,
        "dev1",
        vec![PointValue {
            point_id: 101,
            point_key: "temp".into(),
            value: NGValue::Float64(23.5),
        }],
    ));

    // 2. Configure MappedJson
    let mut mapping = MappedJsonConfig::new();
    // Map device name from "device.name" in input
    mapping.insert("device.id".to_string(), "device.name".to_string());
    // Map value (using JMESPath filter on values array)
    // Input has `data.Telemetry.values`. Find item with point_key='temp' and take value.
    mapping.insert(
        "readings.temperature".to_string(),
        "data.Telemetry.values[?point_key=='temp'].value | [0]".to_string(),
    );
    // Static value or event kind (event_kind is at root)
    mapping.insert("meta.type".to_string(), "event_kind".to_string());

    let config = UplinkPayloadConfig::MappedJson { config: mapping };

    // 3. Build Context
    let ctx = build_context(
        1,
        "test-app",
        "pulsar",
        UplinkEventKind::Telemetry,
        &data,
        &(runtime.clone() as Arc<dyn ng_gateway_sdk::NorthwardRuntimeApi>),
    )
    .expect("failed to build context");

    // 4. Encode
    let encoded = encode_uplink_payload(
        &config,
        &ctx,
        &data,
        &(runtime as Arc<dyn ng_gateway_sdk::NorthwardRuntimeApi>),
    )
    .expect("encoding failed");

    // 5. Verify
    let output: Value = serde_json::from_slice(&encoded).expect("invalid json");

    // Use to_string() for "telemetry" because event_kind might be serialized as string.
    // EnvelopeKind serializes to string (snake_case usually).
    // Let's check output.

    assert_eq!(output["device"]["id"], "dev1");
    assert_eq!(output["readings"]["temperature"], 23.5);
    // event_kind for Telemetry is "telemetry"
    assert_eq!(output["meta"]["type"], "telemetry");
}

#[test]
fn test_downlink_mapped_json() {
    // 1. Setup Downlink Route with nested JMESPath expressions
    let mut mapping = MappedJsonConfig::new();
    mapping.insert("request_id".to_string(), "header.req_id".to_string());
    mapping.insert("point_id".to_string(), "body.params.pid".to_string());
    mapping.insert("value".to_string(), "body.params.val".to_string());
    mapping.insert("timestamp".to_string(), "header.ts".to_string());

    let route = DownlinkRoute {
        kind: DownlinkKind::WritePoint,
        mapping: EventDownlink {
            enabled: true,
            topic: "cmd".to_string(),
            payload: DownlinkPayloadConfig::MappedJson {
                config: mapping,
                filter: MappedDownlinkFilterConfig::None,
            },
            ack_policy: AckPolicy::OnSuccess,
            failure_policy: FailurePolicy::Drop,
        },
    };

    // 2. Input Message with nested structure
    let now_str = chrono::Utc::now().to_rfc3339();
    let input_json = json!({
        "header": {
            "req_id": "req-nested-123",
            "ts": now_str
        },
        "body": {
            "cmd": "write",
            "params": {
                "pid": 101,
                "val": 55.5
            }
        }
    });
    let input_bytes = serde_json::to_vec(&input_json).unwrap();

    let meta = DownlinkMessageMeta {
        key: None,
        properties: None,
    };

    // 3. Decode
    let event = decode_event(&route, &meta, &input_bytes)
        .expect("decode failed")
        .expect("filter should match");

    // 4. Verify
    if let NorthwardEvent::WritePoint(wp) = event {
        assert_eq!(wp.request_id, "req-nested-123");
        assert_eq!(wp.point_id, 101);
        assert_eq!(wp.value, NGValue::Float64(55.5));
    } else {
        panic!("unexpected event type");
    }
}

#[test]
fn test_downlink_mapped_json_with_filter() {
    // 1. Setup Filtered Route
    let mut mapping = MappedJsonConfig::new();
    mapping.insert("request_id".to_string(), "rid".to_string());
    mapping.insert("point_id".to_string(), "pid".to_string());
    mapping.insert("value".to_string(), "v".to_string());
    mapping.insert("timestamp".to_string(), "ts".to_string());

    let route = DownlinkRoute {
        kind: DownlinkKind::WritePoint,
        mapping: EventDownlink {
            enabled: true,
            topic: "cmd".to_string(),
            payload: DownlinkPayloadConfig::MappedJson {
                config: mapping,
                filter: MappedDownlinkFilterConfig::JsonPointer {
                    pointer: "/type".to_string(),
                    equals: "write".to_string(),
                },
            },
            ack_policy: AckPolicy::OnSuccess,
            failure_policy: FailurePolicy::Drop,
        },
    };

    let meta = DownlinkMessageMeta {
        key: None,
        properties: None,
    };

    let now_str = chrono::Utc::now().to_rfc3339();

    // 2. Test Match
    let match_json = json!({
      "type": "write",
      "rid": "r1",
      "pid": 200,
      "v": 10,
      "ts": now_str
    });
    let match_bytes = serde_json::to_vec(&match_json).unwrap();

    let event = decode_event(&route, &meta, &match_bytes)
        .expect("decode failed")
        .expect("should match");

    if let NorthwardEvent::WritePoint(wp) = event {
        assert_eq!(wp.request_id, "r1");
        assert_eq!(wp.point_id, 200);
        // JSON "10" (integer) -> deserializes to Int64(10) via NGValue deserializer
        assert_eq!(wp.value, NGValue::Int64(10));
    } else {
        panic!("unexpected event");
    }

    // 3. Test Mismatch
    let mismatch_json = json!({ "type": "read", "rid": "r1", "pid": 200, "v": 10, "ts": now_str });
    let mismatch_bytes = serde_json::to_vec(&mismatch_json).unwrap();

    let result = decode_event(&route, &meta, &mismatch_bytes).expect("decode shouldn't fail");
    assert!(result.is_none(), "should be ignored due to filter");
}
