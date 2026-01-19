# Point / ActionParameter：`data_type` 作为 wire 语义 + `Transform`（含 scale/offset/negate/TransformDatatype）重构计划（破坏性变更版）

> 目标：把“内存布局类型（wire）”与“逻辑类型 + 工程量换算（transform）”彻底解耦，并统一到 Point 与 Action Parameter 两条链路上。
>
> **硬约束**：本文不考虑兼容旧函数/旧 API/旧容错，允许一次性重构与删除。

---

## 1. 最终模型：语义与计算规则（最佳实践口径）

### 1.1 核心语义（必须写死）

- **`data_type: DataType`**

  - **语义**：**wire_data_type**（内存布局/协议源类型）。
  - 例：Modbus Holding Register 两字节的无符号整型就是 `UInt16`；四字节 IEEE754 就是 `Float32`。

- **Transform（逻辑层变换：类型转换 + 仿射换算）**

  - **语义**：逻辑层变换（类型转换 + 仿射换算）。
  - **Point**：API/SDK 推荐以 `transform: Transform` 暴露；DB 用 `transform_*` 结构化列存储。
  - **ActionParameter / Parameter**：为与 Point 字段命名对齐，JSON 里用 `transform_*` 平铺字段表达；运行时可组装为 `Option<Transform>` 参与 codec 计算。
  - **规则**：未配置 transform（或 transform 字段全空）时，逻辑类型默认 **跟随 wire**；不做数值仿射。

- **`TransformDatatype`（建议命名：`data_type` 或 `logical_data_type`，本文用 `logical_dt` 表示）**
  - **语义**：**逻辑数据类型**（northward 输出 `NGValue` 的类型、UI 校验与展示的类型、写入 API 的类型）。
  - **规则**：`logical_dt = transform.datatype.unwrap_or(wire_dt)`（其中 `wire_dt == point.data_type`）。

### 1.2 Transform 结构（建议）

为保持最小复杂度但覆盖现场需求，本期仅支持仿射 + 逻辑类型：

- **Transform**
  - **datatype**: `Option<DataType>`（逻辑类型；None 表示跟随 wire）
  - **scale**: `Option<f64>`（默认为 1.0）
  - **offset**: `Option<f64>`（默认为 0.0）
  - **negate**: `bool`（默认为 false）

> 说明：把旧 `scale` **完全纳入 transform**，Point/Parameter 不再单独保存 `scale` 字段（破坏性变更）。

### 1.3 Uplink（采集上行）统一算法

给定：

- `wire_dt = point.data_type`
- `logical_dt = transform.datatype.unwrap_or(wire_dt)`（transform 可能为 None）

流程：

1. **Decode（wire）**：按 `wire_dt` 从协议数据解码得到原始值 `x`
2. **Transform（可选）**：仅当 `wire_dt` 与 `logical_dt` 都是数值类时：
   - \(y = x \cdot s + o\)，若 `negate=true` 则 \(y = -y\)
3. **Coerce（logical）**：将 `y` 装箱为 `NGValue(logical_dt)`
4. **Bounds**：`min_value/max_value` 作用在逻辑值 \(y\) 上

### 1.4 Downlink（写入下行）统一算法（闭环必须）

写入请求传入的是 **逻辑类型** `NGValue(logical_dt)`：

1. `validate_datatype(logical_dt)`
2. Bounds（逻辑值）
3. **inverse transform（若启用 transform 且数值型）**
   - 若 `negate=true`：`y = -y`
   - 若 `scale == 0`：拒绝写（配置错误）
   - \(x = (y - o) / s\)
4. **Encode（wire）**：按 `wire_dt` 编码写入设备（整数 wire 采用 `round()` 并范围检查）

---

## 2. 数据结构落库与 API（Point + Action）

### 2.1 Point（DB 是结构化列；API/SDK 是 Transform struct）

当前 `point` 表已有列：`data_type`, `scale` 等。新方案为破坏性重构：

- **保留列名 `data_type`，但语义改为 wire_dt**
- **删除列 `scale`（或至少从业务语义上废弃）**
- 新增一组“transform 列”（对应 Transform struct）：
  - `transform_data_type SMALLINT NULL`（逻辑类型）
  - `transform_scale DOUBLE NULL`
  - `transform_offset DOUBLE NULL`
  - `transform_negate BOOLEAN NOT NULL DEFAULT 0`

并在 API/SDK 上呈现为：

- `transform: Transform`

> 为什么 Point 不直接存 JSON：point 是高频、需要分页过滤/展示/导入导出，结构化列更可控。

#### 2.1.1 Migration（必须补齐；否则线上 DB 无法落库/回读）

本仓库当前仅有 `ng-gateway-storage/src/migration/m20220101_000001_create_table.rs`（一次性 create tables），因此要把 point 的 `transform_*` 列落到 DB，必须新增一个真正的“表结构变更 migration”：

- **新增 migration 文件**：例如 `ng-gateway-storage/src/migration/m20260120_000002_point_transform.rs`
- **在 `ng-gateway-storage/src/migration/mod.rs` 注册**：把新 migration 加到 `MigratorTrait::migrations()` 的列表中（在 create_table 之后）
- **Up（表结构）**：
  - `ALTER TABLE point ADD COLUMN transform_data_type SMALLINT NULL`
  - `ALTER TABLE point ADD COLUMN transform_scale DOUBLE NULL`
  - `ALTER TABLE point ADD COLUMN transform_offset DOUBLE NULL`
  - `ALTER TABLE point ADD COLUMN transform_negate BOOLEAN NOT NULL DEFAULT 0`
- **Up（数据回填，可选但强烈建议）**：
  - 若旧 `point.scale` 存在：`transform_scale = scale`（把旧语义整体迁移进 transform）
  - `transform_data_type` 留空（None 表示 logical 跟随 wire），除非你希望把历史点强制升级为某个固定 logical 类型
- **Up（删除旧列 scale）**：
  - **SQLite**：不支持直接 `DROP COLUMN`，只能“重建表 + copy data + rename”（或接受保留 `scale` 列但业务废弃它）
  - **Postgres/MySQL**：可直接 drop（若部署环境确定不是 SQLite）
- **Down**：按需要实现（破坏性升级可选择不提供 down，或仅撤销新列；SQLite 回退同样需要重建表）

> 破坏性升级口径下：推荐至少做到“新增 transform\_\* + 回填 transform_scale”，至于是否物理删除 `scale` 取决于 DB 后端与发布窗口。

### 2.2 ActionParameter（inputs 是 JSON，但字段也要与 Point 对齐为“平铺语义字段”）

`action.inputs` 目前是 JSON（`Parameters(Vec<Parameter>)`），因此 Parameter 直接扩展字段即可：

- `Parameter.data_type`：语义改为 wire_dt（与 point 同口径）
- **新增一组 transform 平铺字段（与 Point 列命名对齐）**：
  - `Parameter.transform_data_type: Option<DataType>`（logical；None 表示跟随 wire）
  - `Parameter.transform_scale: Option<f64>`
  - `Parameter.transform_offset: Option<f64>`
  - `Parameter.transform_negate: bool`（默认 false）

> 说明：Parameter 仍然是 JSON（没有 DB schema migration 压力），但为了全链路一致性与 UI/模板字段对齐，仍采用 `transform_*` 平铺命名；运行时可用 helper 将平铺字段组装为 `Option<Transform>` 传给 codec。

---

## 3. SDK 级重构（删除旧 API，统一 Transform）

### 3.1 新增公共类型：`Transform`

在 `ng-gateway-sdk` 定义 Transform（供 southward + web/ui schema + 运行时复用）：

- `ng-gateway-sdk/src/southward/types.rs` 或新文件 `ng-gateway-sdk/src/transform.rs`

### 3.2 ValueCodec 重构（强制统一入口）

**删除**：

- `ValueCodec::apply_scale(value, scale)`
- 所有仅带 `scale` 的 `coerce_*_to_value(value, expected, scale)` 入口

**新增**（推荐两层 API）：

- **Transform 纯函数**

  - `ValueCodec::apply_transform_f64(x: f64, t: &Transform) -> f64`
  - `ValueCodec::invert_transform_f64(y: f64, t: &Transform) -> Option<f64>`（scale==0 返回 None）

- **Typed coercion（最终装箱）**
  - `ValueCodec::coerce_bool_to_value(value: bool, logical_dt: DataType, t: Option<&Transform>)`
  - `ValueCodec::coerce_f64_to_value(value: f64, logical_dt: DataType, t: Option<&Transform>)`
  - `ValueCodec::coerce_u64_to_value(value: u64, logical_dt: DataType, t: Option<&Transform>)`
  - `ValueCodec::coerce_i64_to_value(value: i64, logical_dt: DataType, t: Option<&Transform>)`

> 关键点：coerce 的 “expected” 始终是 **logical_dt**，不再混入 wire 语义。

### 3.3 RuntimePoint / RuntimeParameter trait（破坏性升级）

`ng-gateway-sdk/src/southward/mod.rs`：

- `RuntimePoint::data_type()` 语义改为 **wire_dt**
- 新增：
  - `fn transform(&self) -> Option<&Transform>;`
- `RuntimeParameter::data_type()` 语义改为 **wire_dt**
- 新增：
  - `fn transform(&self) -> Option<&Transform>;`

---

## 4. 逐驱动重构清单（含行级/调用点级替换清单）

> 说明：下面的“行号”以当前仓库快照为准（你刚才 grep/打开的版本）。如果后续代码移动，需重新跑一次 grep 校准。

### 4.1 Modbus（`ng-gateway-southward/modbus`）

#### 4.1.1 设计要点

- `point.data_type` == wire_dt（严格决定寄存器宽度与符号）
- `logical_dt` 来自 `point.transform.datatype`（或 None 跟随 wire）
- 删除 smart-cast/长度容错（这类容错是歧义根源）

#### 4.1.2 行级/调用点级变更（旧 → 新）

- **`ng-gateway-southward/modbus/src/driver.rs:L341-L347`（Telemetry）** 与 **`L366-L372`（Attribute）**

  - **旧**：`ModbusCodec::parse_register_value(slice, p.data_type, ..., p.scale)`
  - **新**：
    - wire decode：`wire_dt = p.data_type()`
    - `logical_dt = p.transform().and_then(|t| t.datatype).unwrap_or(wire_dt)`
    - `ModbusCodec::decode_registers_to_ng_value(slice, wire_dt, logical_dt, ..., p.transform())`

- **`ng-gateway-southward/modbus/src/driver.rs:L721-L727`（write_point encode）**

  - **旧**：`encode_registers_from_value(&value, point.data_type, ..., point.quantity.max(1))`
  - **新**：`encode_registers_from_value(&value /* logical */, wire_dt=point.data_type, ..., point.quantity.max(1), point.transform)`

- **`ng-gateway-southward/modbus/src/planner.rs:L133-L139` / `L154-L160`**
  - **旧**：`encode_registers_from_value(value, mp.data_type, ...)`（mp.data_type 之前被当逻辑）
  - **新**：`encode_registers_from_value(value /* logical */, wire_dt=mp.data_type /* wire */, ..., mp.transform)`

#### 4.1.3 codec 必须删除/新增

- **删除**：`modbus/src/codec.rs:parse_register_value(...)`（旧签名：data_type=逻辑 + len 容错）
- **新增**：
  - `decode_registers_to_scalar(words, wire_dt, byte_order, word_order) -> DriverResult<DecodedScalar>`
  - `decode_registers_to_ng_value(words, wire_dt, logical_dt, byte_order, word_order, t: Option<&Transform>) -> DriverResult<NGValue>`
  - `encode_registers_from_value(value: &NGValue /* logical */, wire_dt, byte_order, word_order, quantity, t: Option<&Transform>) -> DriverResult<Vec<u16>>`

---

### 4.2 S7（`ng-gateway-southward/s7`）

- **`ng-gateway-southward/s7/src/driver.rs:L158`**

  - **旧**：`S7Codec::to_value(&v, p.data_type(), p.scale())`
  - **新**：
    - `wire_dt = p.data_type()`（现在表示 wire）
    - `logical_dt = p.transform().and_then(|t| t.datatype).unwrap_or(wire_dt)`
    - `S7Codec::to_value(&v, wire_dt, logical_dt, p.transform())`

- **`ng-gateway-southward/s7/src/codec.rs:L22-L26`**
  - **旧签名**：`to_value(value, expected /*逻辑*/, scale)`
  - **新签名**：`to_value(value, wire_dt, logical_dt, t: Option<&Transform>)`

---

### 4.3 MC（三菱 3E，`ng-gateway-southward/mc`）

- **`ng-gateway-southward/mc/src/driver.rs:L179`**

  - `words_for_data_type(...)` 参数语义变为 `wire_dt`

- **`ng-gateway-southward/mc/src/driver.rs:L300-L308` / `L336-L342`**

  - **旧**：`TypedPointReadSpec { data_type: point.data_type, ... }`（之前混逻辑）
  - **新**：`TypedPointReadSpec { data_type: point.data_type /* wire */, ... }`
  - 逻辑输出类型从 `point.transform.datatype` 决定（在 decode 后 coerce 时使用）

- **`ng-gateway-southward/mc/src/driver.rs:L327` / `L658`**
  - word_len 计算必须用 wire_dt（即 `point.data_type`）

---

### 4.4 IEC104（`ng-gateway-southward/iec104`）

IEC104 的 wire 类型多由 ASDU TypeID 决定；`point.data_type` 作为 wire_dt 更适合做一致性断言：

- 目前调用点（示例）：
  - `ng-gateway-southward/iec104/src/driver.rs:L239-L243` 等多处 `ValueCodec::coerce_*`
  - **旧**：`coerce_*(v, meta.data_type, meta.scale)`
  - **新**：
    - `wire_dt = meta.data_type`（现在是 wire）
    - `logical_dt = meta.transform.datatype.unwrap_or(wire_dt)`
    - `ValueCodec::coerce_*(v, logical_dt, meta.transform.as_ref())`

---

### 4.5 OPC UA（`ng-gateway-southward/opcua`）

- **`ng-gateway-southward/opcua/src/driver.rs:L394-L395`**

  - **旧**：`OpcUaCodec::coerce_variant_value(variant, p.data_type(), p.scale())`
  - **新**：
    - `wire_dt = p.data_type()`（wire）
    - `logical_dt = p.transform().and_then(|t| t.datatype).unwrap_or(wire_dt)`
    - `OpcUaCodec::coerce_variant_value(variant, wire_dt, logical_dt, p.transform())`

- **订阅回调**
  - `ng-gateway-southward/opcua/src/types.rs:L430-L432` 的 `PointMeta::coerce()` 同样改造

---

### 4.6 EtherNet/IP（`ng-gateway-southward/ethernet-ip`）

- **`ng-gateway-southward/ethernet-ip/src/driver.rs:L191-L195`**

  - **旧**：`EthernetIpCodec::to_ng_value(plc_value, point.data_type, point.scale)`
  - **新**：
    - `wire_dt = point.data_type`（wire）
    - `logical_dt = point.transform.datatype.unwrap_or(wire_dt)`
    - `EthernetIpCodec::to_ng_value(plc_value, wire_dt, logical_dt, point.transform.as_ref())`

- **`ng-gateway-southward/ethernet-ip/src/driver.rs:L360`（write_point）**
  - **旧**：`EthernetIpCodec::to_plc_value(&value, point.data_type)`（写逻辑类型到 PLC）
  - **新**：
    - 先校验 `value` 是 logical_dt
    - inverse transform 得到 wire 值
    - `to_plc_value(&wire_value, wire_dt)`

---

### 4.7 DNP3（`ng-gateway-southward/dnp3`）

- `dnp3/src/types.rs:PointMeta`：新增 transform 信息（可用 `transform: Transform`，或同样展开为 `transform_*` 平铺字段）
- `dnp3/src/codec.rs:L20-L34`：coerce 使用逻辑类型 + transform
- `dnp3/src/handler.rs:L128/L152/L188/L206/L224/L242`：调用不变或按新签名替换（取决于是否保留 Dnp3Codec 薄封装）

---

### 4.8 DL/T 645（`ng-gateway-southward/dlt645`）

- `dlt645/src/codec.rs:L74-L93`：coerce 改为逻辑类型 + transform
- `dlt645/src/driver.rs:L839-L846`：write_point 校验应改为校验 logical_dt（来自 transform）

---

### 4.9 CJ/T 188（`ng-gateway-southward/cjt188`）

- `cjt188/src/codec/di_parser.rs:L236-L243`：coerce 改为逻辑类型 + transform

---

## 5. UI / 导入模板字段（破坏性变更）

### 5.1 Point 表单

- 原 `dataType` 字段现在表示：**wire_data_type**
- 新增：`transform` 分组
  - `transform.datatype`（TransformDatatype / 逻辑类型）
  - `transform.scale`
  - `transform.offset`
  - `transform.negate`

### 5.2 Action Parameter 表单

- `param.data_type` 现在表示：wire_dt
- 新增 `param.transform_*`（平铺字段，与 Point 对齐）：
  - `param.transform_data_type`
  - `param.transform_scale`
  - `param.transform_offset`
  - `param.transform_negate`

---

## 6. 示例（现场场景）

### 示例：寄存器 U16=201，逻辑值 20.1

- `point.data_type`（wire_dt）：`UInt16`
- `point.transform`：
  - `datatype`: `Float64`
  - `scale`: `0.1`
  - `offset`: `0`
  - `negate`: `false`

uplink：decode 得到 `x=201` → transform 得到 `y=20.1` → 输出 `NGValue::Float64(20.1)`
