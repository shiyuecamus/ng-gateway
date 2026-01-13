## ng-plugin-kafka

`ng-plugin-kafka` 是 ng-gateway 的 **Kafka 北向插件**（动态库 `cdylib`），实现方式对齐 `ng-plugin-pulsar`：

- **热路径**：`process_data()` 只做编码与入队（无网络 I/O）
- **publisher task**：统一负责 Kafka produce + delivery receipt（有界 inflight）
- **supervisor**：负责连接状态机、重连与（可选）downlink consumer 生命周期

### 构建

在仓库根目录执行：

```bash
cargo build -p ng-plugin-kafka --release
```

产物会在 `target/release/` 下（不同平台扩展名不同）：

- macOS：`libng_plugin_kafka.dylib`
- Linux：`libng_plugin_kafka.so`

### 运行方式（动态加载）

网关会从数据库 `plugin.path` 读取插件动态库的 **绝对路径** 并加载。

### 配置概览

插件配置结构与 UI schema 由 `src/config.rs` / `src/metadata.rs` 定义，核心字段：

- `connection.bootstrapServers`
- `connection.security.protocol`（`plaintext` / `ssl` / `sasl_plaintext` / `sasl_ssl`）
- `uplink.*`（按事件类型配置 topic/key/payload）
- `downlink.*`（按事件类型配置 topic/payload/ackPolicy/failurePolicy）


