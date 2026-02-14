/// Use jemalloc as the global memory allocator on Linux.
///
/// This replaces glibc's default malloc, which is known for aggressive arena
/// pre-allocation (up to 8 × CPU cores × 64 MB), causing inflated RSS in
/// containerized (Docker/Linux) environments.
///
/// The `unprefixed_malloc_on_supported_platforms` feature ensures that
/// jemalloc also exports global `malloc`/`free` symbols, so all dynamically
/// loaded cdylib drivers and plugins (via `dlopen`) transparently inherit
/// jemalloc through the linker's symbol resolution order.
///
/// On macOS this is intentionally **disabled** because macOS uses two-level
/// namespaces for dylib symbol resolution — `.dylib` plugins always bind
/// `malloc`/`free` to `libSystem.B.dylib` regardless of what the host
/// exports. Enabling jemalloc on macOS would cause a cross-allocator
/// mismatch (host frees with jemalloc what the cdylib allocated with
/// system malloc), leading to immediate SEGFAULT on driver/plugin load.
/// macOS's built-in allocator is already efficient, so no action is needed.
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use casbin::CachedEnforcer;
use clap::Parser;
use ng_gateway_common::{
    casbin::{service::NGCasbinService, NGPermChecker},
    event::NGEventBus,
    NGAppContext,
};
use ng_gateway_core::NGGateway;
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{constants::DEFAULT_CONFIG_FILE_NAME, event::ApplicationReady, EventBus};
use ng_gateway_storage::{NGCacheProvider, NGDbManager};
use ng_gateway_web::NGWebServer;
use std::{env::current_dir, path::PathBuf};

/// NG Gateway - High-performance IoT gateway platform
///
/// A high-throughput, high-concurrency IoT gateway that supports multiple
/// southbound protocols (Modbus, S7, IEC104, OPC UA) and northbound
/// communication via MQTT v5.
#[derive(Parser)]
#[command(name = "ng-gateway")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "NG Gateway", long_about = None)]
struct Cli {
    /// Sets a custom config file with full path
    ///
    /// If not specified, the gateway will look for 'gateway.toml'
    /// in the current working directory.
    #[arg(short, long, env = "NG_CONFIG")]
    config: Option<PathBuf>,
}

/// Asynchronous main function for the NG Gateway application.
///
/// Initializes the application context, loads configuration, and starts
/// all gateway services including web server, data collection, and
/// northbound communication.
///
/// # Returns
/// * `NGResult<()>` - Returns `Ok(())` on successful execution, or an error
///   if initialization or runtime fails.
#[tokio::main(flavor = "multi_thread")]
async fn main() -> NGResult<()> {
    let cli = Cli::parse();

    // Determine the configuration file path
    // If not provided via CLI or environment variable, use default path
    let config_path = match cli.config {
        Some(p) => p,
        None => {
            let dir = current_dir()
                .map_err(|e| NGError::from(format!("Failed to get current directory: {e}")))?;
            dir.join(DEFAULT_CONFIG_FILE_NAME)
        }
    };

    // Convert PathBuf to String for NGAppContext::init
    let config_path_str = config_path.to_string_lossy().to_string();

    // Initialize the application context with all required components
    NGAppContext::init::<
        NGEventBus,
        NGDbManager,
        NGCacheProvider,
        NGWebServer,
        NGGateway,
        NGPermChecker,
        NGCasbinService<CachedEnforcer>,
    >(config_path_str)
    .await?;

    // Get the application context instance
    let ctx = NGAppContext::instance().await;

    // Publish ApplicationReady event to notify all subscribers
    let event_bus = ctx.event_bus()?;
    let bus = event_bus.downcast_ref::<NGEventBus>().ok_or(NGError::from(
        "Failed to downcast event bus to NGEventBus (unexpected context wiring)",
    ))?;
    bus.publish::<ApplicationReady>(ApplicationReady).await?;

    // Run the application until shutdown signal is received
    ctx.run().await
}
