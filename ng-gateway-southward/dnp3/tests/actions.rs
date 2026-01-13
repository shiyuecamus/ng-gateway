mod common;

use anyhow::Context;
use common::{
    build_init_context, build_test_actions, build_test_topology, init_tracing, CapturingPublisher,
};
use ng_driver_dnp3::driver::Dnp3Driver;
use ng_gateway_sdk::{
    validate_and_resolve_action_inputs, Driver, RuntimeAction, SouthwardConnectionState,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};

/// Integration test for DNP3 actions (Control).
///
/// Requires a running DNP3 Outstation simulator at 127.0.0.1:20000.
/// Config: Master Addr 1, Outstation Addr 1024.
/// Points:
/// - Binary Input (Group 1, Index 0)
/// - Analog Input (Group 30, Index 0)
#[tokio::test]
#[ignore]
async fn dnp3_execute_actions() -> anyhow::Result<()> {
    init_tracing();

    let host = "8.155.150.180";
    let port = 20000;
    let master_addr = 2;
    let outstation_addr = 1;

    let (channel, device, points) = build_test_topology(host, port, master_addr, outstation_addr);
    let actions = build_test_actions(device.id);
    let publisher = Arc::new(CapturingPublisher::new());

    // We cast to the trait object type expected by build_init_context
    let pub_trait: Arc<dyn ng_gateway_sdk::NorthwardPublisher> = publisher.clone();

    let (ctx, device_arc, _runtime_points) = build_init_context(channel, device, points, pub_trait);

    let driver = Dnp3Driver::with_context(ctx).context("Failed to create DNP3 driver")?;

    driver.start().await.context("Failed to start driver")?;

    // Wait for connection
    let mut conn_rx = driver.subscribe_connection_state();
    let wait_connect = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(*conn_rx.borrow(), SouthwardConnectionState::Connected) {
                break;
            }
            if conn_rx.changed().await.is_err() {
                break;
            }
        }
    })
    .await;

    if wait_connect.is_err() {
        driver.stop().await?;
        anyhow::bail!(
            "Timeout waiting for DNP3 connection. Ensure simulator is running at {}:{}",
            host,
            port
        );
    }
    tracing::info!("DNP3 Connected! Waiting for association to be ready...");

    // Wait for association to be available (connection may be established before association is added)
    driver
        .wait_association(Duration::from_secs(5))
        .await
        .context("Timeout waiting for DNP3 association to be ready")?;

    tracing::info!("DNP3 Association ready! Executing actions...");

    // 1. Test CROB (Pulse On)
    let crob_action = actions
        .iter()
        .find(|a| a.command == "control_relay")
        .context("CROB action not found")?;

    // Pulse On (value = true, or specific op type)
    // let params = json!(true);

    // tracing::info!("Executing CROB LatchOn...");
    // let result = driver
    //     .execute_action(
    //         device_arc.as_ref(),
    //         crob_action as &dyn RuntimeAction,
    //         Some(params),
    //     )
    //     .await;

    // assert!(result.is_ok(), "CROB LatchOn failed: {:?}", result.err());

    // 2. Test CROB (Pulse Off / Open) via string (scalar-only path)
    let params_off = json!("PulseOff");

    tracing::info!("Executing CROB PulseOff...");
    let inputs = crob_action.input_parameters();
    let typed = validate_and_resolve_action_inputs(&inputs, &Some(params_off))?;
    let result = driver
        .execute_action(
            Arc::clone(&device_arc),
            Arc::new(crob_action.clone()),
            typed,
        )
        .await;

    assert!(result.is_ok(), "CROB PulseOff failed: {:?}", result.err());

    // 3. Test Analog Output
    let ao_action = actions
        .into_iter()
        .find(|a| a.command == "analog_output")
        .context("Analog Output action not found")?;

    let ao_params = json!(224.912);

    tracing::info!("Executing Analog Output...");
    let inputs = ao_action.input_parameters();
    let typed = validate_and_resolve_action_inputs(&inputs, &Some(ao_params))?;
    let result = driver
        .execute_action(Arc::clone(&device_arc), Arc::new(ao_action), typed)
        .await;

    assert!(result.is_ok(), "Analog Output failed: {:?}", result.err());

    // Stop driver
    driver.stop().await.context("Failed to stop driver")?;
    Ok(())
}
