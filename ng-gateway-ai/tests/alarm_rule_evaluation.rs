//! Integration tests: multi-rule alarm evaluation scenarios.
//!
//! Verifies that `evaluate_alarm_rules` correctly evaluates multiple
//! independent rules against a pipeline context, producing alarms
//! only for conditions that are actually satisfied.

#![cfg(feature = "engine")]

use bytes::Bytes;
use ng_gateway_ai::pipeline::context::PipelineContext;
use ng_gateway_ai::result::alarm::evaluate_alarm_rules;
use ng_gateway_ai::DecodedFrame;
use ng_gateway_models::domain::prelude::{AlarmRuleInfo, BoundingBox, Detection};
use ng_gateway_models::entities::ai::alarm_rule::AlarmCondition;
use ng_gateway_models::enums::ai::AlarmSeverity;
use std::sync::Arc;

// ── Helpers ──────────────────────────────────────────────────────────

fn make_solid_frame(width: u32, height: u32, r: u8, g: u8, b: u8) -> DecodedFrame {
    let pixel_count = width as usize * height as usize;
    let mut data = Vec::with_capacity(pixel_count * 3);
    for _ in 0..pixel_count {
        data.extend_from_slice(&[r, g, b]);
    }
    DecodedFrame::from_rgb24(Bytes::from(data), width, height)
}

fn make_detection(
    class: &str,
    class_id: u32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    confidence: f32,
) -> Detection {
    Detection {
        bbox: BoundingBox {
            x_min: x1,
            y_min: y1,
            x_max: x2,
            y_max: y2,
        },
        class: Arc::from(class),
        class_id,
        confidence,
        track_id: None,
    }
}

fn make_rule(name: &str, condition: AlarmCondition) -> AlarmRuleInfo {
    AlarmRuleInfo {
        id: 1,
        name: name.to_string(),
        pipeline_id: 1,
        rule_order: 0,
        severity: AlarmSeverity::Warning,
        condition,
        cooldown_secs: 0,
        min_duration_secs: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn context_with_detections(detections: Vec<Detection>) -> PipelineContext {
    let frame = make_solid_frame(640, 480, 0, 0, 0);
    let mut ctx = PipelineContext::new(frame);
    ctx.detections = detections;
    ctx
}

// ── Test 1: Multiple rules evaluate independently ────────────────────

#[test]
fn multiple_rules_evaluate_independently() {
    // Scene: 2 "person" detections inside a zone, 1 "car" outside.
    let detections = vec![
        // Person inside zone (center at (0.3, 0.3)).
        make_detection("person", 0, 0.2, 0.2, 0.4, 0.4, 0.9),
        // Person inside zone (center at (0.5, 0.5)).
        make_detection("person", 0, 0.4, 0.4, 0.6, 0.6, 0.85),
        // Car outside zone (center at (0.05, 0.05)).
        make_detection("car", 1, 0.0, 0.0, 0.1, 0.1, 0.8),
    ];
    let ctx = context_with_detections(detections);

    // Rule 1: ClassDetected("person") — should trigger (2 persons present).
    let rule_class = make_rule(
        "detect_person",
        AlarmCondition::ClassDetected {
            class: "person".into(),
            min_confidence: 0.5,
        },
    );

    // Rule 2: CountExceeds(class=None, threshold=5) — should NOT trigger (only 3 total).
    let rule_count = make_rule(
        "crowd_alert",
        AlarmCondition::CountExceeds {
            class: None,
            threshold: 5,
        },
    );

    // Rule 3: ZoneIntrusion in square (0.2, 0.2) → (0.8, 0.8) for "person" — should trigger.
    let zone = vec![(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)];
    let rule_zone = make_rule(
        "zone_person",
        AlarmCondition::ZoneIntrusion {
            zone,
            class: Some("person".into()),
        },
    );

    let rules = [rule_class, rule_count, rule_zone];
    let alarms = evaluate_alarm_rules(&rules, &ctx);

    // Expect exactly 2 alarms: ClassDetected + ZoneIntrusion.
    // CountExceeds should NOT trigger (3 <= 5).
    assert_eq!(
        alarms.len(),
        2,
        "expected 2 alarms (ClassDetected + ZoneIntrusion), got {}",
        alarms.len()
    );

    let alarm_types: Vec<&str> = alarms.iter().map(|a| a.alarm_type.as_ref()).collect();
    assert!(
        alarm_types.contains(&"detect_person"),
        "ClassDetected rule should trigger"
    );
    assert!(
        alarm_types.contains(&"zone_person"),
        "ZoneIntrusion rule should trigger"
    );
    assert!(
        !alarm_types.contains(&"crowd_alert"),
        "CountExceeds(5) should NOT trigger with 3 detections"
    );
}

// ── Test 2: No rules → no alarms ────────────────────────────────────

#[test]
fn no_rules_no_alarms() {
    let detections = vec![
        make_detection("person", 0, 0.1, 0.1, 0.5, 0.5, 0.9),
        make_detection("car", 1, 0.5, 0.5, 0.9, 0.9, 0.8),
    ];
    let ctx = context_with_detections(detections);

    let alarms = evaluate_alarm_rules(&[], &ctx);
    assert!(
        alarms.is_empty(),
        "no rules should produce no alarms, got {}",
        alarms.len()
    );
}

// ── Test 3: All rules trigger on crowded scene ───────────────────────

#[test]
fn all_rules_trigger_on_crowded_scene() {
    // Create a crowded scene: 10 "person" detections all inside a zone.
    let zone = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    let detections: Vec<Detection> = (0..10)
        .map(|i| {
            let offset = i as f32 * 0.08;
            make_detection(
                "person",
                0,
                0.1 + offset,
                0.1 + offset,
                0.18 + offset,
                0.18 + offset,
                0.9 - i as f32 * 0.01,
            )
        })
        .collect();
    let ctx = context_with_detections(detections);

    // Rule 1: ClassDetected("person") — should trigger.
    let rule_class = make_rule(
        "detect_person",
        AlarmCondition::ClassDetected {
            class: "person".into(),
            min_confidence: 0.5,
        },
    );

    // Rule 2: CountExceeds(class="person", threshold=5) — should trigger (10 > 5).
    let rule_count = make_rule(
        "crowd_alert",
        AlarmCondition::CountExceeds {
            class: Some("person".into()),
            threshold: 5,
        },
    );

    // Rule 3: ZoneIntrusion (entire frame zone) for "person" — should trigger.
    let rule_zone = make_rule(
        "zone_intrusion",
        AlarmCondition::ZoneIntrusion {
            zone,
            class: Some("person".into()),
        },
    );

    let rules = [rule_class, rule_count, rule_zone];
    let alarms = evaluate_alarm_rules(&rules, &ctx);

    assert_eq!(
        alarms.len(),
        3,
        "all 3 rules should trigger, got {}",
        alarms.len()
    );

    let alarm_types: Vec<&str> = alarms.iter().map(|a| a.alarm_type.as_ref()).collect();
    assert!(alarm_types.contains(&"detect_person"));
    assert!(alarm_types.contains(&"crowd_alert"));
    assert!(alarm_types.contains(&"zone_intrusion"));
}

// ── Test 4: No detections → no alarm triggers ────────────────────────

#[test]
fn no_detections_no_alarms() {
    let ctx = context_with_detections(vec![]);

    let rule = make_rule(
        "detect_person",
        AlarmCondition::ClassDetected {
            class: "person".into(),
            min_confidence: 0.5,
        },
    );

    let alarms = evaluate_alarm_rules(&[rule], &ctx);
    assert!(alarms.is_empty(), "no detections should produce no alarms");
}

// ── Test 5: Wrong class → no alarm ───────────────────────────────────

#[test]
fn wrong_class_does_not_trigger() {
    let detections = vec![make_detection("car", 1, 0.1, 0.1, 0.5, 0.5, 0.9)];
    let ctx = context_with_detections(detections);

    let rule = make_rule(
        "detect_person",
        AlarmCondition::ClassDetected {
            class: "person".into(),
            min_confidence: 0.5,
        },
    );

    let alarms = evaluate_alarm_rules(&[rule], &ctx);
    assert!(alarms.is_empty(), "wrong class should not trigger alarm");
}

// ── Test 6: CountExceeds boundary (count == threshold) ───────────────

#[test]
fn count_exceeds_at_boundary_does_not_trigger() {
    let detections: Vec<Detection> = (0..3)
        .map(|i| {
            make_detection(
                "person",
                0,
                i as f32 * 0.2,
                0.1,
                i as f32 * 0.2 + 0.15,
                0.5,
                0.9,
            )
        })
        .collect();
    let ctx = context_with_detections(detections);

    let rule = make_rule(
        "crowd_alert",
        AlarmCondition::CountExceeds {
            class: Some("person".into()),
            threshold: 3,
        },
    );

    let alarms = evaluate_alarm_rules(&[rule], &ctx);
    assert!(
        alarms.is_empty(),
        "count == threshold (3) should NOT trigger (strictly >)"
    );
}
