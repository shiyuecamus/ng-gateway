# ng-gateway-sdk

`ng-gateway-sdk` is the official Rust SDK for building **southward drivers** and **northward plugins**
for NG Gateway.

## What you can build

- **Southward driver** (`cdylib`): connect to field devices/buses (e.g. Modbus, S7, IEC104), implement
  collection / write-point / actions, and export a stable ABI so NG Gateway can load the driver at
  runtime.
- **Northward plugin** (`cdylib`): consume unified `NorthwardData` from the gateway and deliver to
  external platforms (e.g. Kafka, Pulsar, ThingsBoard, HTTP), with backpressure-aware, supervised
  connection lifecycle.

## Key design principles

- **Unified supervision loop**: consistent connection state, retry/backoff, and failure semantics.
- **Hot-path efficiency**: lock-free handle publish, bounded queues, and strongly-typed values
  (`NGValue`) to avoid JSON on critical paths.
- **Product-grade observability**: `tracing` bridge to host, runtime log level governance, and
  structured state snapshots for UI/ops.

## Quick start (driver)

Add dependency:

```toml
[dependencies]
ng-gateway-sdk = "0.1"
```

Export a driver factory via macro:

```rust
use ng_gateway_sdk::ng_driver_factory;

ng_driver_factory!(
    name = "My Driver",
    description = "Demo driver",
    driver_type = "my_driver",
    component = MyConnector,
    metadata_fn = build_metadata,
    model_convert = MyConverter
);
```

## Quick start (plugin)

Add dependency:

```toml
[dependencies]
ng-gateway-sdk = "0.1"
```

Export a plugin factory via macro:

```rust
use ng_gateway_sdk::ng_plugin_factory;

ng_plugin_factory!(
    name = "My Plugin",
    description = "Demo plugin",
    plugin_type = "my_plugin",
    component = MyConnector,
    metadata_fn = build_metadata,
    model_convert = MyConverter
);
```

## License

Apache-2.0

