## ng-plugin-pulsar (Phase 1)

### What’s implemented

- **Uplink (Gateway → Pulsar)**:
  - `DeviceConnected`
  - `DeviceDisconnected`
  - `Telemetry`
  - `Attributes`
- **Payload modes**:
  - `envelope_json` (default, stable envelope JSON)
  - `kv`
  - `timeseries_rows`
- **Topic template**: Handlebars syntax (e.g. `{{event_kind}}`, `{{device_name}}`)
- **Default topic (per your decision)**:
  - `persistent://public/default/ng.uplink.{{event_kind}}.{{device_name}}`
- **Default message key**:
  - `{{device_id}}` (mapped to Pulsar `partition_key`)

### Build output

On macOS, the dynamic library is produced at:

- `target/debug/libng_plugin_pulsar.dylib`

### Install into gateway

The gateway installs northward plugins via the `/plugin/install` API (multipart upload).
It probes the library (`probe_north_library`) and stores metadata in the DB, then loads it into the runtime.

### Minimal config example

```json
{
  "connection": {
    "serviceUrl": "pulsar://127.0.0.1:6650",
    "auth": { "mode": "none" }
  },
  "uplink": {
    "producer": {
      "batchingEnabled": true,
      "batchingMaxMessages": 100,
      "batchingMaxPublishDelayMs": 50
    },
    "telemetry": {
      "enabled": true,
      "topic": "persistent://public/default/ng.uplink.{{event_kind}}.{{device_name}}",
      "key": "{{device_id}}",
      "payload": { "mode": "envelope_json" }
    }
  },
  "downlink": {}
}
```

### EnvelopeJson 深度剖析（Uplink/Downlink 统一契约）

`EnvelopeJson` 是 Pulsar 插件的**稳定消息体协议**，用于：

- **Uplink**（Gateway → Pulsar）：插件将 `NorthwardData` 编码为 Envelope JSON。
- **Downlink**（Pulsar → Gateway）：插件从 Pulsar 收到 JSON 后按同一 Envelope JSON 解码，并将 `payload.data` 反序列化为目标事件结构（`WritePoint` / `Command` / `ServerRpcResponse`）。

#### 1) 顶层结构（schema_version = 1）

```json
{
  "schema_version": 1,
  "event": { "kind": "telemetry" },
  "envelope": {
    "ts_ms": 1734870896000,
    "app": { "id": 1, "name": "my-app", "plugin_type": "pulsar" },
    "device": { "id": 1001, "name": "dev-1", "type": null },
    "channel": { "name": "default" }
  },
  "payload": { "data": {} }
}
```

- `schema_version`: **固定整数**，当前仅支持 `1`（见 downlink 解码检查）。
- `event.kind`: **事件类型**（snake_case 字符串），用于混合 topic 场景下快速路由/忽略。
- `envelope`: **稳定元信息**（时间戳、应用、设备、通道）。
  - `envelope.channel`：无通道信息时为 `null`。
- `payload.data`: **事件负载**（不同事件结构不同）。

#### 2) Uplink：四类事件的完整 EnvelopeJson 示例（消息体输出）

> 说明：下列示例是 **Pulsar 消息 body** 的 JSON 结构（插件实际输出）。  
> 其中 `payload.data` 的字段形状来自代码中的稳定构造逻辑（不会在顶层重复大字段）。

##### 2.1 DeviceConnected (`event.kind = "device_connected"`)

```json
{
  "schema_version": 1,
  "event": { "kind": "device_connected" },
  "envelope": {
    "ts_ms": 1734870896000,
    "app": { "id": 1, "name": "my-app", "plugin_type": "pulsar" },
    "device": { "id": 1001, "name": "dev-1", "type": "thermostat" },
    "channel": null
  },
  "payload": {
    "data": {
      "device_id": 1001,
      "device_name": "dev-1",
      "device_type": "thermostat"
    }
  }
}
```

##### 2.2 DeviceDisconnected (`event.kind = "device_disconnected"`)

```json
{
  "schema_version": 1,
  "event": { "kind": "device_disconnected" },
  "envelope": {
    "ts_ms": 1734870897000,
    "app": { "id": 1, "name": "my-app", "plugin_type": "pulsar" },
    "device": { "id": 1001, "name": "dev-1", "type": "thermostat" },
    "channel": null
  },
  "payload": {
    "data": {
      "device_id": 1001,
      "device_name": "dev-1",
      "device_type": "thermostat"
    }
  }
}
```

##### 2.3 Telemetry (`event.kind = "telemetry"`)

```json
{
  "schema_version": 1,
  "event": { "kind": "telemetry" },
  "envelope": {
    "ts_ms": 1734870900000,
    "app": { "id": 1, "name": "my-app", "plugin_type": "pulsar" },
    "device": { "id": 1001, "name": "dev-1", "type": null },
    "channel": { "name": "modbus-rtu-1" }
  },
  "payload": {
    "data": {
      "timestamp_ms": 1734870900000,
      "values": [
        { "point_id": 10, "point_key": "temp", "value": 23.7 },
        { "point_id": 11, "point_key": "status", "value": "OK" },
        { "point_id": 12, "point_key": "running", "value": true }
      ],
      "metadata": {
        "source": "collector",
        "qos": 0
      }
    }
  }
}
```

##### 2.4 Attributes (`event.kind = "attributes"`)

```json
{
  "schema_version": 1,
  "event": { "kind": "attributes" },
  "envelope": {
    "ts_ms": 1734870905000,
    "app": { "id": 1, "name": "my-app", "plugin_type": "pulsar" },
    "device": { "id": 1001, "name": "dev-1", "type": null },
    "channel": { "name": "modbus-rtu-1" }
  },
  "payload": {
    "data": {
      "timestamp_ms": 1734870905000,
      "client": [{ "point_id": 20, "point_key": "fw_ver", "value": "1.0.3" }],
      "shared": [
        { "point_id": 21, "point_key": "location", "value": "floor-2" }
      ],
      "server": [{ "point_id": 22, "point_key": "threshold", "value": 80 }]
    }
  }
}
```

#### 3) Downlink：三类事件的完整 EnvelopeJson 输入示例（发布到 Pulsar 的 JSON）

> 说明：下列示例是你要 **发布到 Pulsar 的 message body**（插件作为 consumer 收到后解码）。  
> Downlink `EnvelopeJson` 会做两件关键事情：
>
> - 先检查 `schema_version == 1`
> - 再用 `event.kind` 与路由槽位期望值比对：不匹配时 **Ok(None)**（忽略，不报错），适用于**一个 topic 混合多种事件**的场景。

##### 3.1 WritePoint (`event.kind = "write_point"`)

`payload.data` 会反序列化为 `ng_gateway_sdk::WritePoint`：

```json
{
  "schema_version": 1,
  "event": { "kind": "write_point" },
  "payload": {
    "data": {
      "request_id": "req-001",
      "point_id": 101,
      "value": 42,
      "timestamp": "2025-12-22T12:34:56Z",
      "timeout_ms": 5000
    }
  }
}
```

> `value` 类型说明：`WritePoint.value` 是 `NGValue`，其 JSON 表示默认是**标量**：
>
> - 数字 → `Int64/UInt64/Float64`（按 JSON number 解析）
> - 布尔 → `Boolean`
> - 字符串 → `String`（二进制可用 base64 字符串表达）

##### 3.2 CommandReceived (`event.kind = "command_received"`)

`payload.data` 会反序列化为 `ng_gateway_sdk::Command`（注意 `target_type` 是 **UPPERCASE**）：

```json
{
  "schema_version": 1,
  "event": { "kind": "command_received" },
  "payload": {
    "data": {
      "command_id": "cmd-001",
      "key": "reboot",
      "target_type": "GATEWAY",
      "device_id": 1001,
      "device_name": "dev-1",
      "params": { "delay_ms": 1000 },
      "timeout_ms": 10000,
      "timestamp": "2025-12-22T12:35:30Z"
    }
  }
}
```

##### 3.3 RpcResponseReceived (`event.kind = "rpc_response_received"`)

`payload.data` 会反序列化为 `ng_gateway_sdk::ServerRpcResponse`（注意 `target_type` 是 **UPPERCASE**）：

```json
{
  "schema_version": 1,
  "event": { "kind": "rpc_response_received" },
  "payload": {
    "data": {
      "request_id": "rpc-001",
      "target_type": "SUBDEVICE",
      "result": { "ok": true, "latency_ms": 12 },
      "error": null,
      "timestamp": "2025-12-22T12:36:10Z"
    }
  }
}
```
