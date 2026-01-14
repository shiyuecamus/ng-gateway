mod common;

use common::{
    build_init_context, build_modbus_tcp_topology, init_tracing, TestPointConfig, SLAVE_ID,
    SLAVE_IP, SLAVE_PORT,
};
use ng_driver_modbus::{types::ModbusFunctionCode, ModbusDriver};
use ng_gateway_sdk::{AccessMode, DataType, Driver, NorthwardData, SouthwardConnectionState};
use std::{sync::Arc, time::Duration};

// 测试持续时间（秒）
const TEST_DURATION_SECS: u64 = 30;
// 采集周期（秒）
const COLLECT_INTERVAL_SECS: u64 = 2;

// 定义测试点位
fn get_test_points() -> Vec<TestPointConfig> {
    vec![
        // 示例 1: 读取保持寄存器地址 0 (HR0)
        TestPointConfig {
            name: "hr_0".to_string(),
            address: 0,
            quantity: 1,
            function_code: ModbusFunctionCode::ReadHoldingRegisters,
            data_type: DataType::UInt16,
            access_mode: AccessMode::ReadWrite,
        },
        // 示例 2: 读取线圈地址 0 (Coil0)
        // TestPointConfig {
        //     name: "coil_0".to_string(),
        //     address: 0,
        //     quantity: 1,
        //     function_code: ModbusFunctionCode::ReadCoils,
        //     data_type: DataType::Boolean,
        //     access_mode: AccessMode::ReadWrite,
        // },
        // 示例 3: 读取浮点数 (HR10, 2个寄存器)
        TestPointConfig {
            name: "float_10".to_string(),
            address: 0,
            quantity: 2,
            function_code: ModbusFunctionCode::ReadHoldingRegisters,
            data_type: DataType::Float32,
            access_mode: AccessMode::Read,
        },
    ]
}

#[tokio::test]
#[ignore]
async fn test_modbus_tcp_collect() {
    init_tracing();

    tracing::info!("Starting Modbus TCP Collection Test...");
    tracing::info!("Target: {}:{} (Slave {})", SLAVE_IP, SLAVE_PORT, SLAVE_ID);

    // 1. 构建拓扑
    let point_configs = get_test_points();
    let (channel, device, points) =
        build_modbus_tcp_topology(SLAVE_IP, SLAVE_PORT, SLAVE_ID, point_configs);

    // 2. 初始化驱动上下文
    let (ctx, device_arc, runtime_points) = build_init_context(channel, device, points);

    // 3. 创建驱动实例
    let driver = match ModbusDriver::with_context(ctx) {
        Ok(d) => d,
        Err(e) => {
            panic!("Failed to create ModbusDriver: {e}");
        }
    };

    // 4. 启动驱动
    if let Err(e) = driver.start().await {
        panic!("ModbusDriver start failed: {e}");
    }
    tracing::info!("Driver started.");

    // 5. 等待连接成功
    tracing::info!("Waiting for connection...");
    let mut conn_rx = driver.subscribe_connection_state();
    let wait_res = tokio::time::timeout(Duration::from_secs(10), async move {
        loop {
            let state = conn_rx.borrow().clone();
            tracing::debug!("Current connection state: {:?}", state);
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
        panic!(
            "Timeout waiting for connection to {}:{}. Please check your simulator.",
            SLAVE_IP, SLAVE_PORT
        );
    }
    tracing::info!("Connected to Modbus Slave!");

    // 6. 循环采集测试
    let start_time = std::time::Instant::now();
    let duration = Duration::from_secs(TEST_DURATION_SECS);
    let mut iteration = 0;

    while start_time.elapsed() < duration {
        iteration += 1;
        tracing::info!("--- Iteration {} ---", iteration);

        tracing::info!("Collecting data...");
        let res = driver
            .collect_data(Arc::clone(&device_arc), runtime_points.clone())
            .await;

        match res {
            Ok(values) => {
                tracing::info!("Collection successful. Received {} batches.", values.len());
                for (i, data) in values.iter().enumerate() {
                    match data {
                        NorthwardData::Telemetry(t) => {
                            tracing::info!("  Batch [{}]: Telemetry values: {:?}", i, t.values);
                        }
                        NorthwardData::Attributes(a) => {
                            tracing::info!(
                                "  Batch [{}]: Attributes: client={:?}, shared={:?}, server={:?}",
                                i,
                                a.client_attributes,
                                a.shared_attributes,
                                a.server_attributes
                            );
                        }
                        _ => {
                            tracing::info!("  Batch [{}]: Other data: {:?}", i, data);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Collection failed: {}", e);
            }
        }

        tokio::time::sleep(Duration::from_secs(COLLECT_INTERVAL_SECS)).await;
    }

    // 7. 停止驱动
    let _ = driver.stop().await;
    tracing::info!("Collection Test finished.");
}
