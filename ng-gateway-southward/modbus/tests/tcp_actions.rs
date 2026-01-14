mod common;

use common::{
    build_init_context, build_modbus_tcp_topology, init_tracing, TestPointConfig, SLAVE_ID,
    SLAVE_IP, SLAVE_PORT,
};
use ng_driver_modbus::{
    types::{ModbusAction, ModbusFunctionCode, ModbusParameter},
    ModbusDriver,
};
use ng_gateway_sdk::{
    AccessMode, DataType, Driver, NGValue, RuntimeAction, SouthwardConnectionState,
};
use std::{sync::Arc, time::Duration};

// 定义动作测试所需的点位 (主要为了让驱动能启动并连接)
// 这里我们只定义一个用于测试写入的点位
fn get_test_points() -> Vec<TestPointConfig> {
    vec![TestPointConfig {
        name: "hr_0".to_string(),
        address: 0,
        quantity: 1,
        function_code: ModbusFunctionCode::ReadHoldingRegisters,
        data_type: DataType::UInt16,
        access_mode: AccessMode::ReadWrite,
    }]
}

// 定义测试动作
fn get_test_action(device_id: i32) -> ModbusAction {
    ModbusAction {
        id: 1,
        device_id,
        name: "write_hr0".to_string(),
        command: "write".to_string(),
        input_parameters: vec![ModbusParameter {
            name: "value".to_string(),
            key: "value".to_string(),
            data_type: DataType::UInt16,
            required: true,
            default_value: None,
            max_value: None,
            min_value: None,
            function_code: ModbusFunctionCode::WriteSingleRegister,
            address: 0,
            quantity: 1,
        }],
    }
}

#[tokio::test]
#[ignore]
async fn test_modbus_tcp_actions() {
    init_tracing();

    tracing::info!("Starting Modbus TCP Action Test...");
    tracing::info!("Target: {}:{} (Slave {})", SLAVE_IP, SLAVE_PORT, SLAVE_ID);

    // 1. 构建拓扑
    let point_configs = get_test_points();
    let (channel, device, points) =
        build_modbus_tcp_topology(SLAVE_IP, SLAVE_PORT, SLAVE_ID, point_configs);

    // 2. 初始化驱动上下文
    let (ctx, device_arc, _runtime_points) = build_init_context(channel, device, points);

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

    // 6. 执行测试动作
    let test_action = Arc::new(get_test_action(device_arc.id()));
    let action_param_def = test_action.input_parameters();

    tracing::info!("Testing execute_action: write 123 to HR0");

    // 构造参数: value = 123 (UInt16)
    let param_value = NGValue::UInt16(123);

    let params = vec![(Arc::clone(&action_param_def[0]), param_value)];

    match driver
        .execute_action(
            Arc::clone(&device_arc),
            Arc::clone(&test_action) as Arc<dyn RuntimeAction>,
            params,
        )
        .await
    {
        Ok(result) => {
            tracing::info!("Action execution succeeded: {:?}", result);
        }
        Err(e) => {
            panic!("Action execution failed: {}", e);
        }
    }

    // 7. 停止驱动
    let _ = driver.stop().await;
    tracing::info!("Action Test finished.");
}
