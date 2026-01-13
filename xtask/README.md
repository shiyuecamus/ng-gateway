# XTask - Build Automation for NG Gateway

This crate provides custom build tasks for the ng-gateway project using the [xtask pattern](https://github.com/matklad/cargo-xtask).

## 📖 What is XTask?

XTask is a Rust community best practice for managing custom build tasks:
- ✅ Pure Rust solution (no shell scripts)
- ✅ Cross-platform compatible (Windows/Linux/macOS)
- ✅ Type-safe and compile-time checked
- ✅ Integrated with Cargo workflow

## 🚀 Usage

### Build with automatic driver deployment

```bash
# Build release binary and deploy drivers
cargo xtask build --profile release

# Or use the shorter alias
cargo build-with-drivers

# Build debug version
cargo xtask build --profile debug
```

### Deploy drivers only

```bash
# Deploy release drivers (without rebuilding)
cargo xtask deploy --profile release

# Or use the alias
cargo deploy-drivers
```

### Clean artifacts

```bash
# Clean build artifacts only
cargo xtask clean

# Clean build artifacts AND deployed drivers
cargo xtask clean --drivers
```

## 📂 Output Structure

After running `cargo xtask build`, you'll have:

```
ng-gateway/
├── target/
│   └── release/
│       ├── ng-gateway-bin          # Main binary
│       ├── libng_driver_iec104.dylib
│       ├── libng_driver_opcua.dylib
│       └── ...
└── drivers/
    └── builtin/
        ├── libng_driver_iec104.dylib  # Deployed!
        ├── libng_driver_opcua.dylib   # Deployed!
        └── ...
```

## 🐳 Docker Integration

Use xtask in your Dockerfile for automated builds:

```dockerfile
# Build stage
FROM rust:1.83 AS builder
WORKDIR /app
COPY . .

# Build and deploy drivers in one step
RUN cargo xtask build --profile release

# Runtime stage
FROM debian:bookworm-slim
COPY --from=builder /app/target/release/ng-gateway-bin /usr/local/bin/
COPY --from=builder /app/drivers /opt/ng-gateway/drivers

CMD ["ng-gateway-bin"]
```

## 🔧 Adding New Commands

To add a new task, edit `xtask/src/main.rs`:

```rust
#[derive(Subcommand)]
enum Commands {
    Build { ... },
    Deploy { ... },
    Clean { ... },
    // Add your new command here
    MyCommand {
        #[arg(short, long)]
        my_option: String,
    },
}
```

## 📚 Related Resources

### Project Documentation
- [QUICKSTART.md](../QUICKSTART.md) - Quick start guide for the entire project
- [BUILDING.md](../BUILDING.md) - Complete build documentation

### XTask Pattern
- [Cargo XTask Pattern](https://github.com/matklad/cargo-xtask)
- [Cargo Aliases Documentation](https://doc.rust-lang.org/cargo/reference/config.html#alias)

