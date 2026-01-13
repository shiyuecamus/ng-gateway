mod common;

use common::{build_init_context, build_test_topology, init_tracing, wait_connected};
use ng_driver_ethernet_ip::driver::EthernetIpDriver;
use ng_gateway_sdk::{AccessMode, Driver, NorthwardData, PointValue};
use std::{collections::HashSet, sync::Arc, time::Duration};

#[tokio::test]
#[ignore]
async fn uplink_collect_data_once() -> anyhow::Result<()> {
    init_tracing();

    // Adjust these values to match your local Ethernet/IP PLC/simulator setup.
    let host = "192.168.139.138".to_string();
    let port: u16 = 44818;
    let slot: u8 = 0;
    let timeout_ms: u64 = 2000;
    // Tags used for the test points; update them according to your PLC/simulator configuration.
    let tags: Vec<String> = vec!["MyTag1".to_string(), "MyTag2".to_string()];

    let (channel, device, points) =
        build_test_topology(host, port, slot, timeout_ms, &tags, AccessMode::Read);
    let (ctx, device_arc, runtime_points) = build_init_context(channel, device, points);

    let driver = EthernetIpDriver::with_context(ctx)
        .map_err(|e| anyhow::anyhow!("failed to create EthernetIpDriver: {e}"))?;

    driver
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("EthernetIpDriver::start failed: {e}"))?;

    wait_connected(driver.subscribe_connection_state(), Duration::from_secs(10)).await?;

    let res = driver
        .collect_data(Arc::clone(&device_arc), runtime_points.clone())
        .await;

    match res {
        Ok(out) => {
            tracing::info!(
                count = out.len(),
                data = ?out,
                "Ethernet/IP collect_data succeeded"
            );

            // Soft assertions:
            // Ensure that returned point keys (if any) belong to requested tags.
            let expected: HashSet<&str> = tags.iter().map(|s| s.as_str()).collect();
            let returned: HashSet<Arc<str>> = extract_point_keys(&out);
            for k in returned {
                if !expected.contains(k.as_ref()) {
                    return Err(anyhow::anyhow!(
                        "unexpected point_key returned by collect_data: {}",
                        k
                    ));
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Ethernet/IP collect_data returned error");
        }
    }

    // Best-effort shutdown.
    let _ = driver.stop().await;
    Ok(())
}

/// Extract all `point_key` values from northward data.
fn extract_point_keys(data: &[NorthwardData]) -> HashSet<Arc<str>> {
    let mut out = HashSet::new();
    for item in data {
        match item {
            NorthwardData::Telemetry(t) => collect_point_keys(&t.values, &mut out),
            NorthwardData::Attributes(a) => {
                collect_point_keys(&a.client_attributes, &mut out);
                collect_point_keys(&a.shared_attributes, &mut out);
                collect_point_keys(&a.server_attributes, &mut out);
            }
            _ => {}
        }
    }
    out
}

fn collect_point_keys(values: &[PointValue], out: &mut HashSet<Arc<str>>) {
    for v in values {
        out.insert(Arc::clone(&v.point_key));
    }
}
