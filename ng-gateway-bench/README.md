# ng-gateway-bench

This crate provides a **scenario-based benchmark tool** for ng-gateway southward drivers.

## What it measures

- **Collection (采集)**: end-to-end `Driver::collect_data()` latency under periodic schedules.
- **Downlink (下发)**: `Driver::write_point()` latency while collectors are running (scenario 7).
- **System summary** (best-effort):
  - process average CPU usage (%)
  - process peak RSS memory (MiB)
  - system-wide network bandwidth (rx+tx, MiB/s)

## Usage

Run scenario \(1..=7\) for both protocols:

```bash
cargo run -p ng-gateway-bench -- --protocol all --scenario 1
```

Run **all scenarios (1..=7)** for all protocols:

```bash
cargo run -p ng-gateway-bench -- --protocol all --all-scenarios
```

Run a single protocol:

```bash
cargo run -p ng-gateway-bench -- --protocol modbus --scenario 3
cargo run -p ng-gateway-bench -- --protocol opcua --scenario 3
```

Export CSV:

```bash
cargo run -p ng-gateway-bench -- --protocol all --scenario 7 --csv ./bench.csv
```

## Defaults (from your environment)

- **OPC UA**
  - endpoint: `opc.tcp://192.168.66.8:53530/OPCUA/SimulationServer`
  - node ids: `ns=3;i=1001..` (1000 points by default)
- **Modbus TCP**
  - host: `8.155.153.52`
  - port: `502`

## Important Modbus mapping note

This benchmark models each Float32 point as **2 Modbus registers** (`quantity = 2`).
By default, it uses `address_step = 2`, so points are mapped like:

- point 0 -> address 0 (registers 0..1)
- point 1 -> address 2 (registers 2..3)
- ...

If your Modbus simulator uses a different layout, override:

- `--modbus-address-base`
- `--modbus-address-step`
