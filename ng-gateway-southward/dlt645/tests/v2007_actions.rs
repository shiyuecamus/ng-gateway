mod common;

use chrono::{TimeZone, Utc};
use common::{build_init_context, build_test_topology_for_version, init_tracing};
use ng_driver_dlt645::{Dl645Action, Dl645Driver, Dl645FunctionCode, Dl645Parameter, Dl645Version};
use ng_gateway_sdk::{
    validate_and_resolve_action_inputs, DataType, Driver, RuntimeAction, SouthwardConnectionState,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};

async fn wait_connected(driver: &Dl645Driver) {
    let mut conn_rx = driver.subscribe_connection_state();
    tokio::time::timeout(Duration::from_secs(10), async move {
        loop {
            let state = conn_rx.borrow().clone();
            if matches!(state, SouthwardConnectionState::Connected) {
                break;
            }
            if conn_rx.changed().await.is_err() {
                break;
            }
        }
    })
    .await
    .expect("timeout waiting for DL/T 645-2007 driver to connect, please check simulator");
}

async fn run_single_action<F>(label: &str, make_action: F, params: serde_json::Value)
where
    F: FnOnce(i32) -> Dl645Action,
{
    init_tracing();

    let host = "127.0.0.1";
    let port: u16 = 5678;
    let device_address = "000000000000";
    let empty_dis: [&str; 0] = [];

    let (channel, device, points) = build_test_topology_for_version(
        Dl645Version::V2007,
        host,
        port,
        device_address,
        &empty_dis,
    );

    let (ctx, device_arc, _runtime_points) = build_init_context(channel, device, points);

    let driver = Dl645Driver::with_context(ctx)
        .unwrap_or_else(|e| panic!("failed to create Dl645Driver (2007): {e}"));

    driver
        .start()
        .await
        .unwrap_or_else(|e| panic!("Dl645Driver::start (2007) failed: {e}"));

    wait_connected(&driver).await;

    let action = make_action(device_arc.id());

    let inputs = action.input_parameters();
    let typed = validate_and_resolve_action_inputs(&inputs, &Some(params))
        .unwrap_or_else(|e| panic!("validate_and_resolve_action_inputs failed: {e}"));
    let res = driver
        .execute_action(Arc::clone(&device_arc), Arc::new(action), typed)
        .await;

    match res {
        Ok(v) => {
            tracing::info!(
                target: "dlt645::test",
                action = %label,
                result = ?v,
                "DLT645-2007 execute_action succeeded"
            );
        }
        Err(e) => {
            tracing::warn!(
                target: "dlt645::test",
                action = %label,
                error = %e,
                "DLT645-2007 execute_action returned error"
            );
        }
    }

    let stop_res = driver.stop().await;
    if let Err(e) = stop_res {
        tracing::warn!(
            target: "dlt645::test",
            action = %label,
            error = %e,
            "Dl645Driver::stop (2007, single action test) returned error"
        );
    }
}

/// Manual integration test: DL/T 645-2007 `execute_action` (broadcast time sync).
#[tokio::test]
#[ignore]
async fn broadcast_time_sync() {
    // Use a fixed wall-clock time so that the encoded YYMMDDhhmmss (25 12 10 20 46 25)
    // can be compared directly with DL/T645 Master Simulator frames.
    let dt = Utc
        .with_ymd_and_hms(2025, 12, 10, 20, 46, 25)
        .single()
        .expect("invalid fixed timestamp for broadcast_time_sync test");
    let ts_secs = dt.timestamp();
    let params = json!({ "timestamp": ts_secs });

    run_single_action(
        "broadcast_time_sync",
        |device_id| Dl645Action {
            id: 1,
            device_id,
            name: "broadcast-time-sync".to_string(),
            command: "broadcastTimeSync".to_string(),
            input_parameters: vec![Dl645Parameter {
                name: "timestamp".to_string(),
                key: "timestamp".to_string(),
                data_type: DataType::Timestamp,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                function_code: Dl645FunctionCode::BroadcastTimeSync,
                di: None,
                decimals: None,
            }],
        },
        params,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn write_data() {
    let params = json!({ "energy": 3.253 });

    run_single_action(
        "write_data",
        |device_id| Dl645Action {
            id: 2,
            device_id,
            name: "write-data".to_string(),
            command: "writeData".to_string(),
            input_parameters: vec![Dl645Parameter {
                name: "energy".to_string(),
                key: "energy".to_string(),
                data_type: DataType::Float64,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                function_code: Dl645FunctionCode::WriteData,
                di: Some(0x0400_0D01),
                // This DI uses three decimal places in DL/T 645-2007.
                decimals: Some(3),
            }],
        },
        params,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn write_address() {
    let params = json!({ "newAddress": "000000000004" });

    run_single_action(
        "write_address",
        |device_id| Dl645Action {
            id: 3,
            device_id,
            name: "write-address".to_string(),
            command: "writeAddress".to_string(),
            input_parameters: vec![Dl645Parameter {
                name: "newAddress".to_string(),
                key: "newAddress".to_string(),
                data_type: DataType::String,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                function_code: Dl645FunctionCode::WriteAddress,
                di: None,
                decimals: None,
            }],
        },
        params,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn freeze() {
    let params = json!({ "pattern": "99999999" });

    run_single_action(
        "freeze",
        |device_id| Dl645Action {
            id: 4,
            device_id,
            name: "freeze".to_string(),
            command: "freeze".to_string(),
            input_parameters: vec![Dl645Parameter {
                name: "pattern".to_string(),
                key: "pattern".to_string(),
                data_type: DataType::String,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                function_code: Dl645FunctionCode::Freeze,
                di: None,
                decimals: None,
            }],
        },
        params,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn update_baud_rate() {
    let params = json!({ "baud": 4800 });

    run_single_action(
        "update_baud_rate",
        |device_id| Dl645Action {
            id: 5,
            device_id,
            name: "update-baud-rate".to_string(),
            command: "updateBaudRate".to_string(),
            input_parameters: vec![Dl645Parameter {
                name: "baud".to_string(),
                key: "baud".to_string(),
                data_type: DataType::UInt32,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                function_code: Dl645FunctionCode::UpdateBaudRate,
                di: None,
                decimals: None,
            }],
        },
        params,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn clear_max_demand() {
    let params = json!({ "dummy": true });

    run_single_action(
        "clear_max_demand",
        |device_id| Dl645Action {
            id: 7,
            device_id,
            name: "clear-max-demand".to_string(),
            command: "clearMaxDemand".to_string(),
            input_parameters: vec![Dl645Parameter {
                name: "dummy".to_string(),
                key: "dummy".to_string(),
                data_type: DataType::Boolean,
                required: true,
                default_value: Some(json!(true)),
                max_value: None,
                min_value: None,
                function_code: Dl645FunctionCode::ClearMaxDemand,
                di: None,
                decimals: None,
            }],
        },
        params,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn clear_meter() {
    let params = json!({ "dummy": true });

    run_single_action(
        "clear_meter",
        |device_id| Dl645Action {
            id: 8,
            device_id,
            name: "clear-meter".to_string(),
            command: "clearMeter".to_string(),
            input_parameters: vec![Dl645Parameter {
                name: "dummy".to_string(),
                key: "dummy".to_string(),
                data_type: DataType::Boolean,
                required: true,
                default_value: Some(json!(true)),
                max_value: None,
                min_value: None,
                function_code: Dl645FunctionCode::ClearMeter,
                di: None,
                decimals: None,
            }],
        },
        params,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn clear_events() {
    let params = json!({ "eventDi": "FFFFFFFF" });

    run_single_action(
        "clear_events",
        |device_id| Dl645Action {
            id: 9,
            device_id,
            name: "clear-events".to_string(),
            command: "clearEvents".to_string(),
            input_parameters: vec![Dl645Parameter {
                name: "eventDi".to_string(),
                key: "eventDi".to_string(),
                data_type: DataType::String,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                function_code: Dl645FunctionCode::ClearEvents,
                di: None,
                decimals: None,
            }],
        },
        params,
    )
    .await;
}
