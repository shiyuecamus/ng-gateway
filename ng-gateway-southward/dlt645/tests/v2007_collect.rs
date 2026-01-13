mod common;

use common::{build_init_context, build_test_topology_for_version, init_tracing};
use ng_driver_dlt645::{Dl645Driver, Dl645Version};
use ng_gateway_sdk::{Driver, SouthwardConnectionState};
use std::{sync::Arc, time::Duration};

/// Manual integration test: DL/T 645-2007 short disconnect and reconnect.
///
/// This test expects a DL/T 645-2007 slave simulator to be running locally on
/// the configured host/port. It starts the `Dl645Driver`, waits for the
/// connection to reach `Connected` state and then performs a 30-second loop
/// issuing one `collect_data` call per second. During this time you can
/// manually stop and restart the simulator to observe reconnect behavior in
/// the driver and gateway logs.
#[tokio::test]
#[ignore]
async fn reconnect_and_collect() {
    // Initialize tracing so that driver and test logs are visible when running
    // this integration test with `--nocapture`.
    init_tracing();
    // Adjust these values to match your local DL/T 645 slave simulator setup.
    let host = "127.0.0.1";
    let port: u16 = 5678;
    let device_address = "000000000000";
    // DI used for the test point; update it according to the simulator
    // configuration if necessary.
    let point_dis = ["00000000"];

    let (channel, device, points) = build_test_topology_for_version(
        Dl645Version::V2007,
        host,
        port,
        device_address,
        &point_dis,
    );

    let (ctx, device_arc, runtime_points) = build_init_context(channel, device, points);

    let driver = match Dl645Driver::with_context(ctx) {
        Ok(d) => d,
        Err(e) => {
            panic!("failed to create Dl645Driver (2007): {e}");
        }
    };

    // Start driver and wait until connection is established.
    if let Err(e) = driver.start().await {
        panic!("Dl645Driver::start (2007) failed: {e}");
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
        panic!("timeout waiting for DL/T 645-2007 driver to connect, please check simulator");
    }

    // Manual observation loop: every second attempt to collect data once.
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
                    target: "dlt645::test",
                    iteration,
                    count = values.len(),
                    data = ?values,
                    "DLT645-2007 collect_data succeeded"
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "dlt645::test",
                    iteration,
                    error = %e,
                    "DLT645-2007 collect_data error"
                );
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // Clean shutdown (best effort).
    let stop_res = driver.stop().await;
    if let Err(e) = stop_res {
        tracing::warn!(
            target: "dlt645::test",
            error = %e,
            "Dl645Driver::stop (2007) returned error"
        );
    }
}
