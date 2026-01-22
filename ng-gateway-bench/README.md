# ng-gateway-bench

`ng-gateway-bench` 是一个 **基于场景的性能基准测试工具**，用于压测 ng-gateway 的南向驱动（当前包含 Modbus / OPC UA），并输出 Markdown 表格结果。

## 测量内容

- **数据采集（Collection）**：周期调度下的端到端 `Driver::collect_data()` 执行情况（在场景频率下持续运行）
- **数据下发（Downlink）**：在采集同时进行 `Driver::write_point()` 的响应时间（场景 7）
- **资源摘要（best-effort）**
  - **CPU 使用(avg)**：当前进程的平均 CPU 使用率（采样平均）
  - **内存使用(peak RSS)**：当前进程在测试窗口内的 RSS 峰值
  - **网络带宽消耗**：整机网卡累计字节数差分/时间（`receive: ... transmit: ...`，注意是**系统级**，非进程级）

## 场景说明

场景定义在代码中（`ng-gateway-bench/src/scenarios.rs`），目前包含：

- **场景 1~6**：只测采集（不同 channel/device/频率组合）
- **场景 7**：采集 + 下发（默认下发 100 个点，默认 50 次）

## 快速开始（常用 demo）

建议优先用 release 构建（吞吐更稳定）：

```bash
cargo build -p ng-gateway-bench --release
./target/release/ng-gateway-bench --help
```

### 跑单个协议 + 单个场景

```bash
./target/release/ng-gateway-bench --protocol modbus --scenario 3
./target/release/ng-gateway-bench --protocol opcua --scenario 3
```

### 跑全协议 + 单个场景

```bash
./target/release/ng-gateway-bench --protocol all --scenario 1
```

### 跑全协议 + 全场景（1..=7）

```bash
./target/release/ng-gateway-bench --protocol all --all-scenarios
```

### 调整预热/测量时长（更接近稳态）

```bash
./target/release/ng-gateway-bench --protocol opcua --scenario 3 --warmup-secs 10 --duration-secs 60
```

### 调整资源采样间隔（更细粒度/更低开销）

```bash
./target/release/ng-gateway-bench --protocol modbus --scenario 3 --sample-interval-ms 200
./target/release/ng-gateway-bench --protocol modbus --scenario 3 --sample-interval-ms 1000
```

### 场景 7：调整下发点数/次数/超时

```bash
./target/release/ng-gateway-bench --protocol all --scenario 7 \
  --downlink-points 100 --downlink-iterations 100 --downlink-timeout-ms 3000
```

## 参数说明（全部参数）

### 运行控制

- **`--protocol <modbus|opcua|all>`**
  - 选择要测试的协议
  - 默认：`all`
- **`--scenario <1..=7>`**
  - 选择单个场景 id
  - 与 `--all-scenarios` 二选一
- **`--all-scenarios`**
  - 顺序执行所有场景（1..=7）
  - 说明：为了结果稳定，默认按场景串行执行，避免互相干扰
- **`--warmup-secs <seconds>`**
  - 预热时长（秒）
  - 预热阶段会正常采集，但**不记录**采集统计；用来避开冷启动抖动（首次连接/缓存/任务调度稳定）
  - 默认：`5`
- **`--duration-secs <seconds>`**
  - 正式测量时长（秒）
  - 默认：`20`
- **`--sample-interval-ms <ms>`**
  - 资源采样间隔（毫秒），用于 CPU/RSS/网络速率的采样汇总
  - 默认：`500`

### Modbus 参数

- **`--modbus-host <ip/hostname>`**：Modbus TCP server 地址，默认：`8.155.153.52`
- **`--modbus-port <u16>`**：端口，默认：`502`
- **`--modbus-slave-id <u8>`**：UnitId/SlaveId，默认：`1`
- **`--modbus-address-base <u16>`**：寄存器起始地址，默认：`0`
- **`--modbus-address-step <u16>`**：点位地址步进（寄存器单位），默认：`2`
  - 说明：本 bench 将 **Float32 映射为 2 个寄存器**（`quantity = 2`），因此默认 step=2
- **`--modbus-tcp-pool-size <u16>`**：每个 channel 的 TCP 连接池大小，默认：`1`

### OPC UA 参数

- **`--opcua-endpoint <url>`**：OPC UA endpoint URL
  - 默认：`opc.tcp://192.168.66.8:53530/OPCUA/SimulationServer`
- **`--opcua-application-name <string>`**：客户端 application name（用于会话/日志标识）
  - 默认：`SimulationServer@shiyuecamus-MacBook-Pro`
- **`--opcua-application-uri <string>`**：客户端 application URI
  - 默认：`urn:shiyuecamus-MacBook-Pro.local:0PCUA:SimulationServer`
- **`--opcua-node-id-start <u32>`**：点位 NodeId 起始（本 bench 使用 `ns=3;i=<start..start+count>`）
  - 默认：`1002`

### 下发参数（仅场景 7 生效）

- **`--downlink-points <usize>`**：下发点位数量，默认：`100`
- **`--downlink-iterations <usize>`**：下发测试次数，默认：`50`
- **`--downlink-timeout-ms <u64>`**：单次 `write_point` 超时（毫秒），默认：`3000`

## 默认环境（与你当前环境一致）

- **OPC UA（Prosys Simulation Server）**
  - endpoint：`opc.tcp://192.168.66.8:53530/OPCUA/SimulationServer`
  - 点位：`ns=3;i=1001..`（通常 1000 个点）
- **Modbus TCP（阿里云模拟器）**
  - host：`8.155.153.52`
  - port：`502`

## 重要说明：Modbus Float32 映射

本 benchmark 将每个 Float32 点位建模为 **2 个 Modbus Holding Registers**（`quantity = 2`），默认地址布局为：

- point 0 -> address 0（寄存器 0..1）
- point 1 -> address 2（寄存器 2..3）
- ...

如果你的 simulator 地址布局不同，请调整：

- `--modbus-address-base`
- `--modbus-address-step`
