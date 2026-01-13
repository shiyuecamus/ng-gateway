//! NG Gateway Core Library
//!
//! This crate provides core functionality for the NG Gateway, including
//! driver management, data collection, event processing, logging, and metrics collection.
//!
//! ## Features
//!
//! - **Data Collection**: High-performance data collection and scheduling system
//! - **Event Processing**: Event-driven architecture with built-in and custom events
//! - **Metrics & Logging**: Comprehensive observability with structured logging
pub mod casbin;
pub mod event;
mod logger;
// pub mod metrics;

// Re-export error types
pub use ng_gateway_error::{NGError, NGResult};

// Legacy app context (keeping for backward compatibility)
use logger::Logger;
use ng_gateway_models::constants::{
    BUILTIN_DIR, CERT_DIR, CUSTOM_DIR, DATA_DIR, DRIVER_DIR, PKI_DIR, PLUGIN_DIR,
};
use ng_gateway_models::initializer::BuiltinSynchronizer;
use ng_gateway_models::{
    settings::Settings, CacheProvider, CasbinService, DbManager, EventBus, Gateway, PermChecker,
    WebServer,
};
use once_cell::sync::OnceCell;
use opentelemetry::{global, metrics::MeterProvider, KeyValue};
use opentelemetry_otlp::{MetricExporter, WithExportConfig};
use opentelemetry_sdk::{
    metrics::{PeriodicReader, SdkMeterProvider},
    Resource,
};
use std::{
    future::Future,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use sysinfo::{Disks, System};
#[cfg(windows)]
use tokio::signal::ctrl_c;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::{info, instrument, span, Level};

static APP_CONTEXT: OnceCell<RwLock<NGAppContext>> = OnceCell::new();

pub struct NGAppContext {
    /// Global settings
    settings: Option<Settings>,
    /// Global logger
    logger: Logger,
    /// Event bus
    event_bus: Option<Arc<dyn EventBus>>,
    /// Database manager
    db_manager: Option<Arc<dyn DbManager>>,
    /// Cache provider
    cache_provider: Option<Arc<dyn CacheProvider>>,
    /// Web server
    web_server: Option<Arc<dyn WebServer>>,
    /// Casbin service
    casbin_service: Option<Arc<dyn CasbinService>>,
    /// Perm checker
    perm_checker: Option<Arc<dyn PermChecker>>,
    /// Northward manager
    gateway: Option<Arc<dyn Gateway>>,
    /// Flag to prevent duplicate shutdowns
    shutting_down: AtomicBool,
    /// Shutdown token
    shutdown_token: CancellationToken,
}

impl NGAppContext {
    #[inline]
    pub async fn instance() -> RwLockReadGuard<'static, NGAppContext> {
        APP_CONTEXT
            .get()
            .expect("NGAppContext is not initialized")
            .read()
            .await
    }

    #[inline]
    pub async fn instance_mut() -> RwLockWriteGuard<'static, NGAppContext> {
        APP_CONTEXT
            .get()
            .expect("NGAppContext is not initialized")
            .write()
            .await
    }

    /// Initializes the global control center instance.
    ///
    /// This method initializes the logger, loads the settings, and
    /// creates a control center instance. It ensures the instance is initialized only once.
    ///
    /// # Arguments
    /// * `config` - A string containing the configuration path or content.
    ///
    /// # Returns
    /// * `NGResult<()>` - Indicates success or returns an error.
    pub async fn init<E, D, P, W, G, C, S>(config: String) -> NGResult<()>
    where
        E: EventBus + 'static,
        D: DbManager + 'static,
        P: CacheProvider + 'static,
        W: WebServer + 'static,
        G: Gateway + 'static,
        C: PermChecker + 'static,
        S: CasbinService + 'static,
    {
        let mut logger = Logger::new(if cfg!(debug_assertions) {
            Some(Level::DEBUG)
        } else {
            Some(Level::INFO)
        });

        // Load settings first so we can apply runtime directory before initializing the logger.
        // This ensures file logs (./logs) and other relative paths resolve under the runtime root.
        let settings = Settings::new(config)?;

        apply_runtime_dir(&settings.general.runtime_dir)?;

        // Initiates logger (after runtime_dir applied).
        logger.initialize()?;

        let span = span!(Level::INFO, "init-app");
        let _guard = span.enter();

        if settings.metrics.enabled {
            Self::init_metrics(&settings);
        }

        // Ensure required runtime directories exist before subsystems start.
        // This must run after settings are loaded so paths can be configured.
        ensure_runtime_directories()?;

        let event_bus = E::init(&settings).await;

        let mut ctx = NGAppContext {
            shutting_down: AtomicBool::new(false),
            shutdown_token: CancellationToken::new(),
            logger,
            settings: Some(settings),
            event_bus: Some(event_bus),
            db_manager: None,
            cache_provider: None,
            web_server: None,
            casbin_service: None,
            perm_checker: None,
            gateway: None,
        };

        // Initiates DBManager
        ctx.init_db_manager::<D>().await?;

        // Initiates CacheProvider
        ctx.init_cache_provider::<P>().await?;

        // Initiates Casbin Service
        ctx.init_casbin_service::<S>().await?;

        // Initiates Perm Checker
        ctx.init_perm_checker::<C>();

        // Initiates Northward Manager
        ctx.init_gateway::<G>().await?;

        // Initiates Web Server
        ctx.init_web_server::<W>().await?;

        APP_CONTEXT
            .set(RwLock::new(ctx))
            .map_err(|_| NGError::from("Failed to set NGAppContext"))?;
        Ok(())
    }

    async fn init_db_manager<D: DbManager + 'static>(&mut self) -> NGResult<()> {
        let db_manager = D::init(self.settings()?).await?;

        // Synchronize builtin artifacts (drivers, plugins) with the database
        // This ensures that new artifacts in the image are registered in the DB
        BuiltinSynchronizer::sync(&db_manager.get_connection()?).await?;

        self.db_manager = Some(db_manager);
        info!("Database initialized successfully.");
        Ok(())
    }

    async fn init_cache_provider<P: CacheProvider + 'static>(&mut self) -> NGResult<()> {
        self.cache_provider = Some(P::init(self.settings()?).await?);
        info!("Cache provider initialized successfully.");
        Ok(())
    }

    pub async fn init_web_server<W: WebServer + 'static>(&mut self) -> NGResult<()> {
        self.web_server =
            Some(W::init(self.settings()?, self.perm_checker()?, self.gateway()?).await?);
        info!("Web server initialized successfully.");
        Ok(())
    }

    #[instrument(name = "init-casbin-service", skip(self))]
    async fn init_casbin_service<S: CasbinService + 'static>(&mut self) -> NGResult<()> {
        let db_manager = self.db_manager()?;
        self.casbin_service = Some(S::init(db_manager.get_connection()?).await?);
        info!("Casbin service initialized successfully.");
        Ok(())
    }

    pub fn init_perm_checker<C: PermChecker + 'static>(&mut self) {
        self.perm_checker = Some(C::init());
        info!("Perm checker initialized successfully.");
    }

    pub async fn init_gateway<G: Gateway + 'static>(&mut self) -> NGResult<()> {
        self.gateway = Some(G::init(self.settings()?, self.db_manager()?).await?);
        info!("Northward manager initialized successfully.");
        Ok(())
    }

    fn init_metrics(settings: &Settings) {
        let exporter = MetricExporter::builder()
            .with_tonic()
            .with_endpoint(&settings.metrics.endpoint)
            .build()
            .expect("Failed to create metric exporter");

        let provider = SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(exporter)
                    .with_interval(Duration::from_millis(settings.metrics.export_interval))
                    .build(),
            )
            .with_resource(
                Resource::builder()
                    .with_service_name(settings.metrics.service_name.to_string())
                    .build(),
            )
            .build();

        let meter = provider.meter("system_metrics");
        let _cpu_usage = meter
            .f64_observable_up_down_counter("system.cpu.usage")
            .with_description("CPU usage percentage")
            .with_callback(|observer| {
                let mut sys = System::new_all();
                sys.refresh_all();

                observer.observe(
                    sys.global_cpu_usage() as f64,
                    &[KeyValue::new("type", "cpu")],
                );
            })
            .build();
        let _memory_usage = meter
            .f64_observable_up_down_counter("system.memory.usage")
            .with_description("Memory usage percentage")
            .with_callback(|observer| {
                let mut sys = System::new_all();
                sys.refresh_all();

                observer.observe(
                    ((sys.used_memory() as f64) / (sys.total_memory() as f64)) * 100.0,
                    &[KeyValue::new("type", "memory")],
                );
            })
            .build();
        let _disk_usage = meter
            .f64_observable_up_down_counter("system.disk.usage")
            .with_description("Disk usage percentage")
            .with_callback(|observer| {
                let disks = Disks::new_with_refreshed_list();
                if let Some(root_disk) = disks
                    .list()
                    .iter()
                    .find(|d| d.mount_point() == Path::new("/"))
                {
                    let total = root_disk.total_space();
                    let available = root_disk.available_space();
                    observer.observe(
                        (((total - available) as f64) / (total as f64)) * 100.0,
                        &[KeyValue::new("type", "disk")],
                    );
                }
            })
            .build();
        global::set_meter_provider(provider);
    }

    #[inline]
    /// Gets a reference to the settings
    pub fn settings(&self) -> NGResult<&Settings> {
        self.settings
            .as_ref()
            .ok_or(NGError::from("Settings not initialized"))
    }

    #[inline]
    /// Gets a reference to the event bus
    pub fn event_bus(&self) -> NGResult<Arc<dyn EventBus>> {
        self.event_bus
            .as_ref()
            .ok_or(NGError::from("Event bus not initialized"))
            .map(Arc::clone)
    }

    #[inline]
    /// Gets a reference to the database manager
    pub fn db_manager(&self) -> NGResult<Arc<dyn DbManager>> {
        self.db_manager
            .as_ref()
            .ok_or(NGError::from("Database manager not initialized"))
            .map(Arc::clone)
    }

    /// Gets a reference to the cache provider
    #[inline]
    pub fn cache_provider(&self) -> NGResult<Arc<dyn CacheProvider>> {
        self.cache_provider
            .as_ref()
            .ok_or(NGError::from("Cache provider not initialized"))
            .map(Arc::clone)
    }

    #[inline]
    /// Gets a reference to the web server
    pub fn web_server(&self) -> NGResult<Arc<dyn WebServer>> {
        self.web_server
            .as_ref()
            .ok_or(NGError::from("Web server not initialized"))
            .map(Arc::clone)
    }

    /// Gets a reference to the perm checker
    #[inline]
    pub fn perm_checker(&self) -> NGResult<Arc<dyn PermChecker>> {
        self.perm_checker
            .as_ref()
            .ok_or(NGError::from("Perm checker not initialized"))
            .map(Arc::clone)
    }

    /// Gets a reference to the casbin service
    #[inline]
    pub fn casbin_service(&self) -> NGResult<Arc<dyn CasbinService>> {
        self.casbin_service
            .as_ref()
            .ok_or(NGError::from("Casbin service not initialized"))
            .map(Arc::clone)
    }

    #[inline]
    /// Gets a reference to the gateway
    pub fn gateway(&self) -> NGResult<Arc<dyn Gateway>> {
        self.gateway
            .as_ref()
            .ok_or(NGError::from("Northward manager not initialized"))
            .map(Arc::clone)
    }

    #[inline]
    pub fn change_log_level(&self, level: Level) {
        self.logger.set_level(level);
    }

    /// Starts the gateway and listens for shutdown signals.
    ///
    /// It runs the gateway task asynchronously and listens for Ctrl+C signals
    /// to initiate a graceful shutdown.
    pub async fn run(&self) -> NGResult<()> {
        // Listen for shutdown signals
        self.listen_for_shutdown(async { self.graceful_shutdown().await })
            .await
    }

    // 处理信号函数
    async fn listen_for_shutdown<F>(&self, shutdown_fn: F) -> NGResult<()>
    where
        F: Future<Output = NGResult<()>>,
    {
        let shutdown_token = self.shutdown_token.clone();

        #[cfg(unix)]
        {
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
            let mut sigint =
                signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
            let mut sighup =
                signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");
            let mut sigquit =
                signal(SignalKind::quit()).expect("failed to register SIGQUIT handler");

            tokio::select! {
                _ = sigterm.recv() => {
                    info!("Received SIGTERM signal");
                }
                _ = sigint.recv() => {
                    info!("Received SIGINT signal");
                }
                _ = sighup.recv() => {
                    info!("Received SIGHUP signal");
                }
                _ = sigquit.recv() => {
                    info!("Received SIGQUIT signal");
                }
                _ = shutdown_token.cancelled() => {}
            }
        }

        #[cfg(windows)]
        {
            tokio::select! {
                _ = ctrl_c() => {
                    info!("Received ctrl-c signal");
                }
                _ = shutdown_token.cancelled() => {}
            }
        }

        // 执行关闭逻辑
        shutdown_fn.await
    }

    #[inline]
    #[instrument(name = "graceful-shutdown", skip_all)]
    /// Initiates a graceful shutdown process
    pub async fn graceful_shutdown(&self) -> NGResult<()> {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        info!("🛑 Starting graceful shutdown...");

        let tracker = TaskTracker::new();
        // Shutdown components in reverse order of initialization
        if let Some(gateway) = &self.gateway {
            let gateway = Arc::clone(gateway);
            tracker.spawn(async move {
                let _ = gateway.stop().await;
            });
        }
        if let Some(web_server) = &self.web_server {
            let web_server = Arc::clone(web_server);
            tracker.spawn(async move {
                let _ = web_server.stop().await;
            });
        }
        if let Some(db_manager) = &self.db_manager {
            let db_manager = Arc::clone(db_manager);
            tracker.spawn(async move {
                let _ = db_manager.close().await;
            });
        }

        info!("⏳ Waiting for all components to shutdown gracefully...");
        tracker.close();
        tracker.wait().await;

        info!("✅ Graceful shutdown completed successfully");
        std::process::exit(0);
    }
}

/// Apply the configured runtime directory by switching the process working directory.
///
/// # Why this exists
/// The gateway intentionally uses relative paths (`./data`, `./drivers`, `./plugins`, `./certs`...)
/// to keep distributions simple and portable. By setting the process working directory at startup,
/// we can relocate the whole runtime tree without rewriting many path fields.
fn apply_runtime_dir(runtime_dir: &str) -> NGResult<()> {
    let dir = runtime_dir.trim();
    if dir.is_empty() || dir == "." {
        return Ok(());
    }

    std::fs::create_dir_all(dir)
        .map_err(|e| NGError::from(format!("Failed to create runtime_dir {}: {}", dir, e)))?;

    std::env::set_current_dir(dir).map_err(|e| {
        NGError::from(format!(
            "Failed to set current_dir to runtime_dir {}: {}",
            dir, e
        ))
    })?;

    Ok(())
}

/// Ensure required runtime directories exist
///
/// This function creates essential directories used by the gateway at runtime,
/// including data storage and driver/plugin locations for both builtin and custom artifacts.
/// It is safe to call multiple times and will not fail if directories already exist.
fn ensure_runtime_directories() -> NGResult<()> {
    // Compose directory list
    let dirs = [
        // SQLite data directory is always a relative path under the runtime root.
        // `runtime_dir` is applied by `apply_runtime_dir()` before we reach here.
        Path::new(DATA_DIR).to_path_buf(),
        Path::new(CERT_DIR).to_path_buf(),
        Path::new(PKI_DIR).to_path_buf(),
        Path::new(PKI_DIR).join("own"),
        Path::new(PKI_DIR).join("private"),
        Path::new(DRIVER_DIR).to_path_buf(),
        Path::new(PLUGIN_DIR).to_path_buf(),
        Path::new(DRIVER_DIR).join(BUILTIN_DIR),
        Path::new(DRIVER_DIR).join(CUSTOM_DIR),
        Path::new(PLUGIN_DIR).join(BUILTIN_DIR),
        Path::new(PLUGIN_DIR).join(CUSTOM_DIR),
    ];

    for dir in dirs {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return Err(NGError::from(format!(
                "Failed to create directory {}: {}",
                dir.display(),
                e
            )));
        }
    }

    Ok(())
}
