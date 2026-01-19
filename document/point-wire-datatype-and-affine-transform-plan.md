# Point `wire_data_type` + Affine Transform（scale + offset + negate）重构计划（破坏性变更版）

> 目标：把 **“逻辑类型（data_type）”** 与 **“wire/内存布局类型（wire_data_type）”** 彻底拆开，并将缩放从“仅乘 scale”升级为**仿射变换**（`scale + offset + negate`），用于 uplink 工程量换算与 downlink 逆变换写入。
>
> **约束（按你的要求）**：本计划 **不考虑兼容旧函数/旧 API/旧容错**，允许一次性重构与删除。

---

## 1. 最终数据模型（明确语义）

### 1.1 Point 字段语义（全局统一）

- **`data_type: DataType`**

  - **语义**：逻辑数据类型（northward 输出 `NGValue` 的类型、UI 校验与展示的类型、写入 API 的类型）。

- **`wire_data_type: Option<DataType>`**

  - **语义**：驱动 decode/encode 使用的 wire/内存布局类型。
  - **运行时解析规则**：`wire_dt = wire_data_type.unwrap_or(data_type)`（Option 只是表达“跟随逻辑类型”，不是兼容旧 API 的手段）。

- **`scale: Option<f64>` / `offset: Option<f64>` / `negate: bool`**
  - **语义**：对数值型点应用仿射变换（uplink）与逆变换（downlink）。

### 1.2 统一换算公式（uplink）

对数值型（`DataType::is_numeric()`）：

\[
y = \text{negate?}(-(x \cdot s + o)):(x \cdot s + o)
\]

- `x`: 按 `wire_dt` decode 的原始值（中间态统一 `f64`）
- `s`: `scale.unwrap_or(1.0)`
- `o`: `offset.unwrap_or(0.0)`
- `y`: 逻辑值，最终装箱为 `data_type` 的 `NGValue`

非数值型（Boolean/String/Binary/Timestamp）：不应用 affine（保持协议语义）。

### 1.3 downlink 逆变换（必须实现闭环）

对数值型：

1. 若 `negate=true`：`y = -y`
2. `x = (y - o) / s`（`s==0` ⇒ 配置错误，拒绝写）
3. 按 `wire_dt` 编码写入设备（整数 wire 采用 `round()` 并做范围检查）

---

## 2. 破坏性变更清单（删除/重命名/签名变更）

### 2.1 SDK（强制改动点）

- **删除**：`ValueCodec::apply_scale(value, scale)`（仅乘法，语义过窄）
- **删除**：所有仅带 `scale` 的 `coerce_*_to_value(...)` 入口（避免双轨 API）
- **新增/替换**：统一仿射版本（示例命名，具体可按你团队习惯调整）
  - `ValueCodec::apply_affine(value: f64, scale: Option<f64>, offset: Option<f64>, negate: bool) -> f64`
  - `ValueCodec::coerce_bool_to_value(value, expected, scale, offset, negate)`（bool 通常忽略 affine，但签名统一可减少分支）
  - `ValueCodec::coerce_f64_to_value(value, expected, scale, offset, negate)`
  - `ValueCodec::coerce_u64_to_value(value, expected, scale, offset, negate)`
  - `ValueCodec::coerce_i64_to_value(value, expected, scale, offset, negate)`

### 2.2 Southward trait（必须一次性升级）

修改 `ng-gateway-sdk/src/southward/mod.rs` 的 `RuntimePoint`：

- **新增必需方法**（不提供 default impl，强制所有驱动点实现一致语义）
  - `fn wire_data_type(&self) -> Option<DataType>;`
  - `fn offset(&self) -> Option<f64>;`
  - `fn negate(&self) -> bool;`

> 仍保留 `fn data_type(&self) -> DataType; fn scale(&self) -> Option<f64>;`，但所有 codec/driver 必须切换到 affine 版本。

### 2.3 Web/API/UI（不保留旧字段定义）

- `NewPoint/UpdatePoint/PointInfo`：必须新增并暴露 `wireDataType/offset/negate`
- UI（`ng-gateway-ui/apps/web-antd`）：PointForm 必须渲染并提交新字段；导入模板必须支持新列

---

## 3. 仓库级改造点（非驱动）

### 3.1 DB migration（必须）

`point` 表新增列：

- `wire_data_type SMALLINT NULL`
- `offset DOUBLE NULL`
- `negate BOOLEAN NOT NULL DEFAULT 0`

新增 migration 文件并注册（不改历史 create_table）：

- `ng-gateway-storage/src/migration/m20260120_000002_alter_point_add_wire_and_affine.rs`
- `ng-gateway-storage/src/migration/mod.rs` 注册

### 3.2 Models / Repository / Domain

需要把字段贯穿到所有层（这里仅列“必须改的文件”）：

- `ng-gateway-models/src/entities/point.rs`
- `ng-gateway-models/src/domain/point.rs`
- `ng-gateway-repository/src/point.rs`（主要是 partial model / select 字段覆盖）
- `ng-gateway-web/src/api/v1/point.rs`（入参/出参结构自然跟随 domain）
- `ng-gateway-sdk/src/southward/model.rs`（`PointModel` 新增字段）

### 3.3 Core 索引元数据（PointMeta）

`ng-gateway-core/src/southward/manager.rs` 目前构建 `PointMeta { data_type, scale, ... }`（供 northward/monitor 等用）。

- **必须新增**：`wire_data_type/offset/negate`
- **必须明确**：`PointValue.value.data_type()` 与 `PointMeta.data_type`（逻辑）一致；`wire_data_type` 仅用于 southward codec/driver

---

## 4. 逐驱动重构清单（优化/重构/删除点逐项列明）

> 本节是核心：每个驱动列出“改哪些文件、删哪些函数、重构哪些路径、要移除哪些容错”。

### 4.1 Modbus（`ng-gateway-southward/modbus`）——最重构、收益最大

#### 4.1.1 需要新增字段（点模型）

- `modbus/src/types.rs`
  - `ModbusPoint` 增加：`wire_data_type: Option<DataType>`, `offset: Option<f64>`, `negate: bool`
  - `impl RuntimePoint for ModbusPoint`：实现新增 trait 方法

#### 4.1.2 codec 重构（删除旧容错、拆 decode 与 coerce）

- `modbus/src/codec.rs`
  - **删除**：`ModbusCodec::parse_register_value(words, data_type, byte_order, word_order, scale)`
    - 必须移除现有 “len=2 也能读 Float32 / len=8 也能读 UInt16”等 smart-cast/容错分支（这是类型歧义根源）
  - **新增**（建议）：
    - `decode_registers_to_number(words, wire_dt, byte_order, word_order) -> DriverResult<f64>`
      - 依据 `wire_dt` 严格要求 word 数：
        - `Int16/UInt16`：1 word
        - `Int32/UInt32/Float32`：2 words
        - `Int64/UInt64/Float64/Timestamp`：4 words（Timestamp 是否允许由协议决定）
      - 长度不匹配：直接 `ConfigurationError`（拒绝容错）
    - `decode_registers_to_ng_value(words, wire_dt, logical_dt, byte_order, word_order, scale, offset, negate) -> DriverResult<NGValue>`
      - 内部：`decode -> ValueCodec::coerce_f64/u64/i64_to_value(..., logical_dt, scale, offset, negate)`
  - **写入相关删除/替换**：
    - `encode_registers_from_value(&NGValue, data_type, ...)` 必须改成以 `wire_dt` 为准：
      - 新签名：`encode_registers_from_value(&NGValue /* logical */, wire_dt, byte_order, word_order, quantity, scale, offset, negate)`
      - 流程：`logical NGValue -> (y) -> inverse affine -> x -> encode as wire_dt`

#### 4.1.3 driver 重构（collect + write）

- `modbus/src/driver.rs`

  - collect 时：用 `wire_dt` 解码寄存器，再输出 `data_type` 的 `NGValue`
    - 替换调用点：`ModbusCodec::parse_register_value(...)` → `decode_registers_to_ng_value(..., p.wire_data_type.unwrap_or(p.data_type), p.data_type, p.scale, p.offset, p.negate)`
  - write 时：
    - `validate_datatype(point.data_type)` 保持（逻辑校验）
    - 由 `encode_registers_from_value(... wire_dt ...)` 做逆变换 + 编码

- `modbus/src/planner.rs`

  - **必须改动**：所有计算 quantity/word_len 的逻辑必须使用 `wire_dt`（不是 `data_type`）
  - action 参数同理（如果 action 参数未来也需要 affine，可复用同样设计）

- `modbus/tests/*`
  - 更新测试用例：新增 `wire_data_type/offset/negate` 字段；移除依赖旧容错行为的测试

---

### 4.2 S7（`ng-gateway-southward/s7`）——已有 typed value，主要做 affine + wire_dt 断言/映射

#### 4.2.1 点模型字段

- `s7/src/types.rs`
  - `S7Point` 增加：`wire_data_type/offset/negate`
  - `RuntimePoint` 新方法实现

#### 4.2.2 codec 重构（集中在 `S7Codec`）

- `s7/src/codec.rs`
  - **重构**：`S7Codec::to_value(value: &S7DataValue, expected: DataType, scale: Option<f64>)`
  - **新签名**：
    - `to_value(value: &S7DataValue, wire_dt: DataType, logical_dt: DataType, scale, offset, negate) -> Option<NGValue>`
  - **实现语义**：
    - 从 `S7DataValue` 按 **wire_dt** 提取 `x`（例如 Word/DWord/Real/Int 等），类型不匹配直接返回 None（配置错误信号）
    - `x -> affine -> y -> coerce to logical_dt`
  - **写路径**：
    - `S7Codec::from_value(v: &NGValue, ts: S7TransportSize)` 之前必须先把逻辑值做 inverse affine → 按 wire_dt coerce，再转成对应 transport size 的 `S7DataValue`

#### 4.2.3 driver 重构点

- `s7/src/driver.rs`
  - collect 时：从 session/planner 取得 `S7DataValue` 后，调用新版 `S7Codec::to_value(...wire_dt/logical_dt...)`
  - write 时：`validate_datatype(point.data_type)` 保持（逻辑）；编码时使用 `wire_dt + inverse affine`

---

### 4.3 MC（三菱 3E，`ng-gateway-southward/mc`）——强依赖 word_len，必须改为 wire_dt 驱动

#### 4.3.1 点模型字段

- `mc/src/types.rs`
  - `McPoint` 增加：`wire_data_type/offset/negate`

#### 4.3.2 word_len / typed_api / codec 重构

- `mc/src/driver.rs`

  - `words_for_data_type(data_type, string_len_bytes)` **必须改为** `words_for_data_type(wire_dt, string_len_bytes)`（读写长度以 wire 为准）
  - collect 逻辑里 `specs.push(TypedPointReadSpec { data_type: point.data_type, ... })`
    - 必须改为传入 **wire_dt**（用于读多少字、如何解码）

- `mc/src/typed_api.rs`

  - `TypedPointReadSpec` / `McReadItemTyped` 需要携带 `logical_dt` 与 affine 参数（或者直接在 driver 层做二次转换）
  - 推荐：typed_api 只做 **wire decode**，输出一个 `DecodedScalar`（或 `f64`）供 driver 层 affine+coerce

- `mc/src/codec.rs`
  - `encode_typed(data_type, value: &NGValue)` 的 `data_type` 参数必须变为 **wire_dt**
  - 写入点值时：`NGValue(logical) -> inverse affine -> encode(wire_dt)`

---

### 4.4 IEC104（`ng-gateway-southward/iec104`）——消息本身带类型，重点是 affine+logical_dt 输出

#### 4.4.1 点模型字段

- `iec104/src/types.rs`
  - `Iec104Point` 增加：`wire_data_type/offset/negate`
  - `RuntimePoint` 新方法实现

#### 4.4.2 driver 重构（核心改动点清单）

- `iec104/src/driver.rs`
  - 当前大量分支直接 `ValueCodec::coerce_*_to_value(v, meta.data_type, meta.scale)`：
    - 必须替换为 affine 版本：`coerce_*_to_value(v, meta.data_type, meta.scale, meta.offset, meta.negate)`
  - `wire_data_type` 的作用：
    - 对 IEC104：wire 类型更多来自 ASDU TypeID；`wire_dt` 可用于 **断言配置一致性**（例如 TypeID=Float 但 wire_dt 配成 Int，直接告警/丢弃）
    - 即：增加 `assert_wire_dt_matches_typeid(meta.wire_dt, type_id)`，不匹配视作配置错误

---

### 4.5 OPC UA（`ng-gateway-southward/opcua`）——Variant 为 wire，coerce 输出逻辑类型

#### 4.5.1 点模型字段

- `opcua/src/types.rs`
  - `OpcUaPoint` 增加：`wire_data_type/offset/negate`

#### 4.5.2 codec 重构

- `opcua/src/codec.rs`
  - `coerce_variant_value(value, expected, scale)`：
    - 新签名：`coerce_variant_value(value, logical_dt, scale, offset, negate)`
    - 在 numeric path 上调用 affine 版本的 ValueCodec
  - `wire_data_type`：
    - 可选：若 `wire_data_type` 存在，则在 `numeric_as_f64` 前做 Variant 类型断言（例如期待 UInt16 却拿到 Double），不匹配报错/丢弃

#### 4.5.3 driver 重构

- `opcua/src/driver.rs`
  - collect：替换 `OpcUaCodec::coerce_variant_value(variant, p.data_type(), p.scale())`
    - 改为传入 `p.data_type()`（逻辑）+ `p.offset()` + `p.negate()`
  - write：保持 `validate_datatype(point.data_type)`（逻辑），Variant 编码仍以逻辑类型写入（OPC UA 通常写逻辑类型即可）

---

### 4.6 EtherNet/IP（`ng-gateway-southward/ethernet-ip`）——PlcValue 为 wire，输出逻辑类型

#### 4.6.1 点模型字段

- `ethernet-ip/src/types.rs`
  - `EthernetIpPoint` 增加：`wire_data_type/offset/negate`

#### 4.6.2 codec/driver 重构

- `ethernet-ip/src/codec.rs`

  - `to_ng_value(value: PlcValue, expected: DataType, scale)`：
    - 新签名：`to_ng_value(value: PlcValue, logical_dt: DataType, scale, offset, negate)`
    - 各分支用 affine 版本 ValueCodec
  - `wire_data_type`：
    - 可选断言：如果配置了 `wire_data_type`，则校验 `PlcValue` 变体是否匹配（例如 wire=UInt16 却读到 LREAL，直接告警）

- `ethernet-ip/src/driver.rs`
  - collect：替换 `EthernetIpCodec::to_ng_value(plc_value, point.data_type, point.scale)`
    - 改为传入 `offset/negate`
  - write_point：如果你希望“写工程量到整型 tag”，则必须：
    - `logical NGValue -> inverse affine -> coerce to wire_dt -> to_plc_value(wire_dt)`
    - 否则 wire_dt 只是断言，不参与写入

---

### 4.7 DNP3（`ng-gateway-southward/dnp3`）——handler 使用 PointMeta，集中在 meta 与 codec

#### 4.7.1 元数据结构改造

- `dnp3/src/types.rs`
  - `PointMeta` 增加：`wire_data_type/offset/negate`
  - `Dnp3Point` 增加同样字段

#### 4.7.2 codec 重构

- `dnp3/src/codec.rs`
  - `bool_to_value/f64_to_value/u64_to_value` 全部改为 affine 版本
  - `octets_to_value`（binary/string）保持不变

#### 4.7.3 handler/driver 连接处

- `dnp3/src/handler.rs`
  - `buffer_with_meta_lookup` 的闭包仍返回 `Option<NGValue>`，但内部应使用新版 Dnp3Codec（带 offset/negate）

---

### 4.8 DL/T 645（`ng-gateway-southward/dlt645`）——写点已实现，必须闭环支持 inverse affine

#### 4.8.1 点模型字段

- `dlt645/src/types.rs`
  - `Dl645Point` 增加：`wire_data_type/offset/negate`

#### 4.8.2 uplink codec 改造

- `dlt645/src/codec.rs`
  - `decode_point_value` 内所有 `ValueCodec::coerce_f64_to_value(v, point.data_type, point.scale)`
    - 替换为 affine 版本（传入 `point.offset/point.negate`）
  - `wire_data_type`：
    - DL/T 645 的 wire 是 BCD/协议字段；`wire_dt` 可作为“解码后 raw 值类型断言”（比如强制走 u64/i64/f64 路径），但不建议影响报文解析长度（长度由 DI/decimals 决定）

#### 4.8.3 downlink（write_point）闭环

- `dlt645/src/driver.rs`
  - 目前 `write_point` 直接把 `NGValue(logical)` 交给 `encode_parameter_value`
  - 必须改为：`logical -> inverse affine -> raw -> encode`
    - 在 `Dl645Codec::encode_parameter_value` 前，执行 inverse affine 并把值转换成 wire_dt（若 wire_dt 配置为空则跟随 data_type）

---

### 4.9 CJ/T 188（`ng-gateway-southward/cjt188`）——仅 uplink；写点未实现

#### 4.9.1 点模型字段

- `cjt188/src/types.rs`
  - `Cjt188Point` 增加：`wire_data_type/offset/negate`

#### 4.9.2 uplink 解析处改造

- `cjt188/src/codec/di_parser.rs`
  - 当前循环中对每个 point 执行：
    - `ValueCodec::coerce_f64/u64/i64_to_value(v, point.data_type, point.scale)`
  - 必须替换为 affine 版本（传入 `point.offset/point.negate`）
  - `wire_data_type`：
    - 该协议的 wire 值由 DI schema 决定（BCD、u16 状态等），`wire_dt` 建议作为断言/选择分支（DecodedScalar::U64/I64/F64 的选择与 wire_dt 不一致时告警）

---

## 5. 示例（配置与结果）

### 示例：现场（U16=201 → 20.1）

- `data_type`：`Float64`
- `wire_data_type`：`UInt16`
- `scale`：`0.1`
- `offset`：`None`
- `negate`：`false`

uplink：`201 -> 201*0.1 = 20.1`，输出 `NGValue::Float64(20.1)`
