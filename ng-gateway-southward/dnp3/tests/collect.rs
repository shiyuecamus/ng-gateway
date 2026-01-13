mod common;

use anyhow::{bail, Context};
use common::{build_init_context, build_test_topology, init_tracing, CapturingPublisher};
use ng_driver_dnp3::driver::Dnp3Driver;
use ng_gateway_sdk::{Driver, SouthwardConnectionState};
use std::{sync::Arc, time::Duration};

/// Integration test for DNP3 driver.
///
/// Requires a running DNP3 Outstation simulator at 127.0.0.1:20000.
/// Config: Master Addr 1, Outstation Addr 1024.
/// Points:
/// - Binary Input (Group 1, Index 0)
/// - Analog Input (Group 30, Index 0)
#[tokio::test]
#[ignore]
async fn dnp3_connect_and_recv_data() -> anyhow::Result<()> {
    init_tracing();

    let host = "8.155.150.180";
    let port = 20000;
    let master_addr = 2;
    let outstation_addr = 1;

    let (channel, device, points) = build_test_topology(host, port, master_addr, outstation_addr);
    let publisher = Arc::new(CapturingPublisher::new());

    // We cast to the trait object type expected by build_init_context
    let pub_trait: Arc<dyn ng_gateway_sdk::NorthwardPublisher> = publisher.clone();

    let (ctx, _device_arc, _runtime_points) =
        build_init_context(channel, device, points, pub_trait);

    let driver = Dnp3Driver::with_context(ctx).context("Failed to create DNP3 driver")?;

    driver.start().await.context("Failed to start driver")?;

    // Run for a fixed duration after start, then stop.
    let shutdown_sleep = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(shutdown_sleep);

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
        driver
            .stop()
            .await
            .context("Failed to stop driver after connection timeout")?;
        bail!(
            "Timeout waiting for DNP3 connection. Ensure simulator is running at {}:{}",
            host,
            port
        );
    }
    tracing::info!("DNP3 Connected! Streaming data and printing on arrival for up to 60 seconds");

    // Poll captured data and print any new items immediately (best-effort).
    // Note: CapturingPublisher stores data in-memory; we avoid re-printing by
    // tracking the previously observed length.
    let mut interval = tokio::time::interval(Duration::from_millis(200));
    let mut last_len = 0usize;

    loop {
        tokio::select! {
            _ = &mut shutdown_sleep => {
                tracing::info!("Stopping DNP3 driver after 60 seconds");
                break;
            }
            _ = interval.tick() => {
                let data = publisher.get_data();
                let new_len = data.len();
                if new_len > last_len {
                    for d in &data[last_len..new_len] {
                        tracing::info!("Data: {:?}", d);
                    }
                    last_len = new_len;
                }
            }
        }
    }

    driver.stop().await.context("Failed to stop driver")?;
    Ok(())
}
