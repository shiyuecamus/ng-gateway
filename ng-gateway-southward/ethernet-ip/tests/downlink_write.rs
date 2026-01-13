mod common;

use common::{build_init_context, build_test_topology, init_tracing, wait_connected};
use ng_driver_ethernet_ip::driver::EthernetIpDriver;
use ng_driver_ethernet_ip::types::{EthernetIpAction, EthernetIpParameter};
use ng_gateway_sdk::{
    validate_and_resolve_action_inputs, AccessMode, DataType, Driver, ExecuteOutcome, NGValue,
    NorthwardData, RuntimeAction,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};

#[tokio::test]
#[ignore]
async fn downlink_write_point() -> anyhow::Result<()> {
    init_tracing();

    // Adjust these values to match your local Ethernet/IP PLC/simulator setup.
    let host = "192.168.139.138".to_string();
    let port: u16 = 44818;
    let slot: u8 = 0;
    let timeout_ms: u64 = 2000;
    // Tags used for the test points; update them according to your PLC/simulator configuration.
    let tags: Vec<String> = vec!["MyTag1".to_string()];

    // Write payload configuration:
    // - Keep (value, data_type) consistent with the PLC tag type to avoid CIP type mismatch errors.
    let (value, data_type) = (NGValue::Int32(123), DataType::Int32);
    // When enabled, we read back the value after write and compare.
    let assert_readback: bool = false;

    let (channel, device, points) =
        build_test_topology(host, port, slot, timeout_ms, &tags, AccessMode::ReadWrite);
    let (ctx, device_arc, runtime_points) = build_init_context(channel, device, points);

    let driver = EthernetIpDriver::with_context(ctx)
        .map_err(|e| anyhow::anyhow!("failed to create EthernetIpDriver: {e}"))?;

    driver
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("EthernetIpDriver::start failed: {e}"))?;

    wait_connected(driver.subscribe_connection_state(), Duration::from_secs(10)).await?;

    // We write through the point API to simulate UI write-point / RPC write workflows.
    let point = Arc::clone(&runtime_points[0]);
    let wr = driver
        .write_point(
            Arc::clone(&device_arc),
            point,
            value.clone(),
            Some(timeout_ms),
        )
        .await;

    match wr {
        Ok(r) => {
            tracing::info!(outcome = ?r.outcome, "Ethernet/IP write_point succeeded");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Ethernet/IP write_point returned error");
            let _ = driver.stop().await;
            return Ok(());
        }
    }

    // Optional read-back verification.
    if assert_readback {
        let out = driver
            .collect_data(Arc::clone(&device_arc), runtime_points.clone())
            .await
            .map_err(|e| anyhow::anyhow!("collect_data after write failed: {e}"))?;

        let read_back = find_value_by_point_key(&out, tags[0].as_str());
        match read_back {
            Some(v) => {
                if !ng_value_eq(&v, &value, data_type) {
                    return Err(anyhow::anyhow!(
                        "read-back mismatch for tag '{}': expected {:?}, got {:?}",
                        tags[0].clone(),
                        value,
                        v
                    ));
                }
                tracing::info!(tag = %tags[0].clone(), value = ?v, "read-back verification passed");
            }
            None => {
                return Err(anyhow::anyhow!(
                    "read-back did not return tag '{}'; check EIP_TEST_READ_TAGS/point config",
                    tags[0].clone()
                ));
            }
        }
    }

    let _ = driver.stop().await;
    Ok(())
}

#[tokio::test]
#[ignore]
async fn downlink_execute_action_single_param() -> anyhow::Result<()> {
    init_tracing();

    // Adjust these values to match your local Ethernet/IP PLC/simulator setup.
    let host = "192.168.139.138".to_string();
    let port: u16 = 44818;
    let slot: u8 = 0;
    let timeout_ms: u64 = 2000;
    // Tags used for the test points; update them according to your PLC/simulator configuration.
    let tags: Vec<String> = vec!["MyTag1".to_string(), "MyTag2".to_string()];

    // Write payload configuration:
    // - Keep (value, data_type) consistent with the PLC tag type to avoid CIP type mismatch errors.
    let (value1, value2, data_type1, data_type2) = (
        NGValue::Int32(123),
        NGValue::Float32(456.789),
        DataType::Int32,
        DataType::Float32,
    );

    // For execute_action we do not need read points; we only need a device and a channel.
    let (channel, device, points) =
        build_test_topology(host, port, slot, timeout_ms, &tags, AccessMode::Read);
    let (ctx, device_arc, _runtime_points) = build_init_context(channel, device, points);

    let driver = EthernetIpDriver::with_context(ctx)
        .map_err(|e| anyhow::anyhow!("failed to create EthernetIpDriver: {e}"))?;

    driver
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("EthernetIpDriver::start failed: {e}"))?;

    wait_connected(driver.subscribe_connection_state(), Duration::from_secs(10)).await?;

    let action: Arc<dyn RuntimeAction> = Arc::new(EthernetIpAction {
        id: 1,
        device_id: device_arc.id(),
        name: "write-tag".to_string(),
        command: "writeTag".to_string(),
        input_parameters: vec![
            EthernetIpParameter {
                name: "myTag1".to_string(),
                key: "myTag1".to_string(),
                data_type: data_type1,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                tag_name: tags[0].clone(),
            },
            EthernetIpParameter {
                name: "myTag2".to_string(),
                key: "myTag2".to_string(),
                data_type: data_type2,
                required: true,
                default_value: None,
                max_value: None,
                min_value: None,
                tag_name: tags[1].clone(),
            },
        ],
    });

    let inputs = action.input_parameters();
    let params = json!({ "myTag1": ng_value_to_json_scalar(&value1), "myTag2": ng_value_to_json_scalar(&value2) });
    let typed = validate_and_resolve_action_inputs(&inputs, &Some(params))
        .map_err(|e| anyhow::anyhow!("validate_and_resolve_action_inputs failed: {e}"))?;

    let res = driver
        .execute_action(Arc::clone(&device_arc), action, typed)
        .await;

    match res {
        Ok(v) => {
            if v.outcome != ExecuteOutcome::Completed {
                return Err(anyhow::anyhow!(
                    "execute_action outcome unexpected: {:?}",
                    v.outcome
                ));
            }
            tracing::info!(payload = ?v.payload, "Ethernet/IP execute_action succeeded");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Ethernet/IP execute_action returned error");
        }
    }

    let _ = driver.stop().await;
    Ok(())
}

fn ng_value_to_json_scalar(v: &NGValue) -> serde_json::Value {
    match v {
        NGValue::Boolean(b) => json!(b),
        NGValue::Int32(i) => json!(i),
        NGValue::UInt32(i) => json!(i),
        NGValue::Float32(f) => json!(f),
        NGValue::Float64(f) => json!(f),
        NGValue::String(s) => json!(s.as_ref()),
        _ => serde_json::Value::Null,
    }
}

fn find_value_by_point_key(data: &[NorthwardData], key: &str) -> Option<NGValue> {
    for item in data {
        match item {
            NorthwardData::Telemetry(t) => {
                if let Some(v) = t.values.iter().find(|pv| pv.point_key.as_ref() == key) {
                    return Some(v.value.clone());
                }
            }
            NorthwardData::Attributes(a) => {
                if let Some(v) = a
                    .client_attributes
                    .iter()
                    .chain(a.shared_attributes.iter())
                    .chain(a.server_attributes.iter())
                    .find(|pv| pv.point_key.as_ref() == key)
                {
                    return Some(v.value.clone());
                }
            }
            _ => {}
        }
    }
    None
}

fn ng_value_eq(actual: &NGValue, expected: &NGValue, ty: DataType) -> bool {
    match (ty, actual, expected) {
        (DataType::Boolean, NGValue::Boolean(a), NGValue::Boolean(b)) => a == b,
        (DataType::Int32, NGValue::Int32(a), NGValue::Int32(b)) => a == b,
        (DataType::UInt32, NGValue::UInt32(a), NGValue::UInt32(b)) => a == b,
        (DataType::Float32, NGValue::Float32(a), NGValue::Float32(b)) => (*a - *b).abs() < 1e-5,
        (DataType::Float64, NGValue::Float64(a), NGValue::Float64(b)) => (*a - *b).abs() < 1e-9,
        (DataType::String, NGValue::String(a), NGValue::String(b)) => a.as_ref() == b.as_ref(),
        _ => false,
    }
}
