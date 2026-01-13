mod common;

use common::{build_init_context, build_test_topology_for_version, init_tracing};
use ng_driver_cjt188::{types::Cjt188Version, Cjt188Driver};
use ng_gateway_sdk::{Driver, SouthwardConnectionState};
use std::{sync::Arc, time::Duration};

/// Manual integration test: CJ/T 188-2004 short disconnect and reconnect.
///
/// This test expects a CJ/T 188-2004 slave simulator to be running locally on
/// the configured host/port.
#[tokio::test]
#[ignore]
async fn reconnect_and_collect() {
    init_tracing();

    // Adjust these values to match your local CJ/T 188 slave simulator setup.
    let host = "127.0.0.1";
    let port: u16 = 5679;
    // Type 10 (Water) + Logical Address
    let device_address = "00000000EE0001";

    // 901F: Accumulated Flow (Water)
    // Adjust according to simulator capabilities
    let point_dis = ["901F"];

    let (channel, device, points) = build_test_topology_for_version(
        Cjt188Version::V2004,
        host,
        port,
        device_address,
        &point_dis,
    );

    let (ctx, device_arc, runtime_points) = build_init_context(channel, device, points);

    let driver = match Cjt188Driver::with_context(ctx) {
        Ok(d) => d,
        Err(e) => {
            panic!("failed to create Cjt188Driver (2004): {e}");
        }
    };

    if let Err(e) = driver.start().await {
        panic!("Cjt188Driver::start (2004) failed: {e}");
    }

    let mut conn_rx = driver.subscribe_connection_state();
    let wait_res = tokio::time::timeout(Duration::from_secs(10), async move {
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
    .await;

    if wait_res.is_err() {
        panic!("timeout waiting for CJ/T 188-2004 driver to connect, please check simulator");
    }

    let start = std::time::Instant::now();
    let duration = Duration::from_secs(30);
    let mut iteration: u32 = 0;

    while start.elapsed() < duration {
        iteration = iteration.saturating_add(1);
        let res = driver
            .collect_data(Arc::clone(&device_arc), runtime_points.clone())
            .await;
        match res {
            Ok(values) => {
                tracing::info!(
                    target: "cjt188::test",
                    iteration,
                    count = values.len(),
                    data = ?values,
                    "CJT188-2004 collect_data succeeded"
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "cjt188::test",
                    iteration,
                    error = %e,
                    "CJT188-2004 collect_data error"
                );
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let stop_res = driver.stop().await;
    if let Err(e) = stop_res {
        tracing::warn!(
            target: "cjt188::test",
            error = %e,
            "Cjt188Driver::stop (2004) returned error"
        );
    }
}
