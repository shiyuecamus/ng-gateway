use config::{Config, File};
use ng_gateway_error::NGResult;
use serde::{self, Deserialize};
use std::{ops::Deref, sync::Arc};
use sysinfo::System;

use crate::constants::{CERT_DIR, DATA_DIR};

#[derive(Debug, Clone)]
pub struct Settings(Arc<Inner>);

impl Deref for Settings {
    type Target = Inner;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl Settings {
    pub fn new(config_path: String) -> NGResult<Self> {
        let builder = Config::builder()
            .add_source(File::with_name(config_path.as_str()).required(false)) // 加载文件配置
            .add_source(
                config::Environment::with_prefix("NG")
                    .separator("__")
                    .try_parsing(true)
                    .list_separator(",") // list separator
                    .with_list_parse_key("db.cache.redis.cluster.nodes")
                    .with_list_parse_key("web.cors.whitelist.origins")
                    .with_list_parse_key("web.cors.whitelist.methods")
                    .with_list_parse_key("web.cors.whitelist.headers")
                    .with_list_parse_key("web.cors.whitelist.expose_headers"),
            );
        let inner: Inner = builder.build()?.try_deserialize()?;
        Ok(Self(Arc::new(inner)))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Inner {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub web: Web,
    #[serde(default)]
    pub db: Db,
    #[serde(default)]
    pub cache: Cache,
    #[serde(default)]
    pub metrics: Metrics,
}

#[derive(Debug, Clone, Deserialize)]
pub struct General {
    /// Runtime root directory for all relative paths.
    ///
    /// # What this controls
    /// The gateway uses many relative paths by design (e.g. `./data`, `./drivers`, `./plugins`,
    /// `./certs`, `./pki`). This field defines the directory that those relative paths are
    /// resolved from by changing the process working directory at startup.
    ///
    /// # Best practice
    /// - Linux packages (systemd): keep `runtime_dir="."` and rely on `WorkingDirectory=...`.
    /// - Containers/K8s: set an absolute path (e.g. `/var/lib/ng-gateway`) via config or env.
    ///
    /// # Environment override
    /// - `NG__GENERAL__RUNTIME_DIR=/var/lib/ng-gateway`
    #[serde(default = "General::runtime_dir_default")]
    pub runtime_dir: String,
    #[serde(default = "General::ca_cert_path_default")]
    pub ca_cert_path: String,
    #[serde(default = "General::ca_key_path_default")]
    pub ca_key_path: String,
    /// Collection engine configuration
    #[serde(default)]
    pub collector: CollectorConfig,
    /// Northward manager configuration
    #[serde(default)]
    pub northward: Northward,
    /// Southward communication configuration
    #[serde(default)]
    pub southward: Southward,
}

impl Default for General {
    fn default() -> Self {
        General {
            runtime_dir: General::runtime_dir_default(),
            ca_cert_path: General::ca_cert_path_default(),
            ca_key_path: General::ca_key_path_default(),
            collector: CollectorConfig::default(),
            northward: Northward::default(),
            southward: Southward::default(),
        }
    }
}

impl General {
    fn runtime_dir_default() -> String {
        ".".into()
    }

    fn ca_cert_path_default() -> String {
        "ca.crt".into()
    }

    fn ca_key_path_default() -> String {
        "ca.key".into()
    }

    /// Resolve CA certificate path under the runtime root.
    ///
    /// # Rules
    /// - If the configured value looks like a path (contains `/` or starts with `.`),
    ///   it is treated as an explicit path and returned as-is.
    /// - Otherwise it is treated as a file name under `CERT_DIR` (default: `./certs`).
    pub fn ca_cert_path_resolved(&self) -> String {
        resolve_cert_path(&self.ca_cert_path)
    }

    /// Resolve CA private key path under the runtime root.
    ///
    /// See `ca_cert_path_resolved()` for the resolution rules.
    pub fn ca_key_path_resolved(&self) -> String {
        resolve_cert_path(&self.ca_key_path)
    }
}

/// Resolve a certificate-related path under `CERT_DIR` when a bare file name is provided.
fn resolve_cert_path(value: &str) -> String {
    let v = value.trim();
    if v.is_empty() {
        return v.to_string();
    }
    if v.starts_with('.') || v.contains('/') {
        return v.to_string();
    }
    format!("{}/{}", CERT_DIR, v)
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct CollectorConfig {
    /// Collection timeout for each device (in milliseconds)
    #[serde(default = "CollectorConfig::collection_timeout_ms_default")]
    pub collection_timeout_ms: u64,
    /// Metrics collection interval (in milliseconds)
    #[serde(default = "CollectorConfig::metrics_interval_ms_default")]
    pub metrics_interval_ms: u64,
    /// Maximum concurrent collections per channel
    #[serde(default = "CollectorConfig::max_concurrent_collections_default")]
    pub max_concurrent_collections: usize,
    /// Retry attempts for failed collections
    #[serde(default = "CollectorConfig::retry_attempts_default")]
    pub retry_attempts: u32,
    /// Retry delay in milliseconds
    #[serde(default = "CollectorConfig::retry_delay_ms_default")]
    pub retry_delay_ms: u64,
    /// Outbound queue capacity from collector to gateway (bounded channel)
    #[serde(default = "CollectorConfig::outbound_queue_capacity_default")]
    pub outbound_queue_capacity: usize,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        CollectorConfig {
            collection_timeout_ms: CollectorConfig::collection_timeout_ms_default(),
            metrics_interval_ms: CollectorConfig::metrics_interval_ms_default(),
            max_concurrent_collections: CollectorConfig::max_concurrent_collections_default(),
            retry_attempts: CollectorConfig::retry_attempts_default(),
            retry_delay_ms: CollectorConfig::retry_delay_ms_default(),
            outbound_queue_capacity: CollectorConfig::outbound_queue_capacity_default(),
        }
    }
}

impl CollectorConfig {
    fn collection_timeout_ms_default() -> u64 {
        30000
    }

    fn metrics_interval_ms_default() -> u64 {
        60000
    }

    fn max_concurrent_collections_default() -> usize {
        200
    }

    fn retry_attempts_default() -> u32 {
        3
    }

    fn retry_delay_ms_default() -> u64 {
        1000
    }

    fn outbound_queue_capacity_default() -> usize {
        10000
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Web {
    #[serde(default = "Web::router_prefix_default")]
    pub router_prefix: String,
    #[serde(default = "Web::host_default")]
    pub host: String,
    #[serde(default = "Web::port_default")]
    pub port: u16,
    #[serde(default = "Web::workers_default")]
    pub workers: i32,
    /// Static UI (admin console) serving configuration.
    ///
    /// This enables a "single process / single container" distribution model:
    /// the gateway serves both API and the web UI on the same HTTP port.
    #[serde(default)]
    pub ui: WebUi,
    #[serde(default)]
    pub ssl: SSLWithCert,
    #[serde(default)]
    pub cors: Cors,
    #[serde(default)]
    pub jwt: Jwt,
}

impl Default for Web {
    fn default() -> Self {
        Web {
            router_prefix: Web::router_prefix_default(),
            host: Web::host_default(),
            port: Web::port_default(),
            ui: Default::default(),
            ssl: Default::default(),
            cors: Default::default(),
            workers: Web::workers_default(),
            jwt: Default::default(),
        }
    }
}

impl Web {
    fn router_prefix_default() -> String {
        "/api".into()
    }

    fn port_default() -> u16 {
        5678
    }

    fn host_default() -> String {
        "0.0.0.0".into()
    }

    fn workers_default() -> i32 {
        0 // 默认使用 CPU 数量
    }

    /// Get actual number of workers based on configuration
    pub fn get_worker_count(&self) -> usize {
        match self.workers {
            0 => System::new_all().cpus().len(),
            n if n > 0 => n as usize,
            n => std::cmp::max(
                1,
                (System::new_all().cpus().len() as i32 / n.abs()) as usize,
            ),
        }
    }
}

/// Web UI serving configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct WebUi {
    /// Enable serving UI from the gateway process.
    #[serde(default = "WebUi::enabled_default")]
    pub enabled: bool,

    /// Serving mode for UI assets.
    #[serde(default)]
    pub mode: WebUiMode,

    /// Filesystem root directory for UI assets when `mode = "filesystem"`.
    ///
    /// Expected to contain `index.html` plus `css/`, `js/`... (Vite dist output).
    #[serde(default = "WebUi::filesystem_root_default")]
    pub filesystem_root: String,
}

impl Default for WebUi {
    fn default() -> Self {
        Self {
            enabled: WebUi::enabled_default(),
            mode: Default::default(),
            filesystem_root: WebUi::filesystem_root_default(),
        }
    }
}

impl WebUi {
    fn enabled_default() -> bool {
        // Safe default for dev/CI: don't assume UI assets are present.
        true
    }

    fn filesystem_root_default() -> String {
        // Convenient default for developer mode when building UI locally.
        "./ng-gateway-ui/apps/web-antd/dist".into()
    }
}

/// Web UI serving mode.
#[derive(Default, Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebUiMode {
    /// Serve from embedded zip (`ng-gateway-web/ui-dist.zip`) for single-binary distribution.
    EmbeddedZip,
    /// Serve from filesystem directory (`web.ui.filesystem_root`).
    #[default]
    Filesystem,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SSL {
    #[serde(default = "SSL::enabled_default")]
    pub enabled: bool,
    #[serde(default = "SSL::port_default")]
    pub port: u16,
}

impl Default for SSL {
    fn default() -> Self {
        SSL {
            enabled: SSL::enabled_default(),
            port: SSL::port_default(),
        }
    }
}

impl SSL {
    fn enabled_default() -> bool {
        false
    }
    fn port_default() -> u16 {
        8443
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SSLWithCert {
    #[serde(default = "SSLWithCert::enabled_default")]
    pub enabled: bool,
    #[serde(default = "SSLWithCert::port_default")]
    pub port: u16,
    #[serde(default)]
    pub r#type: SSLType,
    #[serde(default)]
    pub cert: String,
    #[serde(default)]
    pub key: String,
}

impl Default for SSLWithCert {
    fn default() -> Self {
        SSLWithCert {
            enabled: SSLWithCert::enabled_default(),
            port: SSLWithCert::port_default(),
            r#type: Default::default(),
            cert: Default::default(),
            key: Default::default(),
        }
    }
}

impl SSLWithCert {
    fn enabled_default() -> bool {
        false
    }

    fn port_default() -> u16 {
        5679
    }
}

#[derive(Default, Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SSLType {
    #[default]
    Auto,
    Local,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct Cors {
    #[serde(default)]
    pub mode: CorsMode,
    #[serde(default)]
    pub whitelist: Whitelist,
}

#[derive(Default, Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorsMode {
    #[default]
    AllowAll,
    Whitelist,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Whitelist {
    #[serde(default = "Whitelist::origins_default")]
    pub origins: Vec<String>,
    #[serde(default = "Whitelist::methods_default")]
    pub methods: Vec<String>,
    #[serde(default = "Whitelist::headers_default")]
    pub headers: Vec<String>,
    #[serde(default = "Whitelist::expose_headers_default")]
    pub expose_headers: Vec<String>,
    #[serde(default = "Whitelist::credentials_default")]
    pub credentials: bool,
}

impl Default for Whitelist {
    fn default() -> Self {
        Whitelist {
            origins: Whitelist::origins_default(),
            methods: Whitelist::methods_default(),
            headers: Whitelist::headers_default(),
            expose_headers: Whitelist::expose_headers_default(),
            credentials: Whitelist::credentials_default(),
        }
    }
}

impl Whitelist {
    fn origins_default() -> Vec<String> {
        vec!["*".into()]
    }

    fn methods_default() -> Vec<String> {
        vec!["GET".into(), "POST".into(), "PUT".into(), "DELETE".into()]
    }

    fn headers_default() -> Vec<String> {
        vec![
            "Content-Type".into(),
            "AccessToken".into(),
            "X-CSRF-Token".into(),
            "Authorization".into(),
            "Token".into(),
            "X-Token".into(),
            "X-User-Id".into(),
        ]
    }

    fn expose_headers_default() -> Vec<String> {
        vec![
            "Content-Length".into(),
            "Access-Control-Allow-Origin".into(),
            "Access-Control-Allow-Headers".into(),
            "Content-Type".into(),
        ]
    }

    fn credentials_default() -> bool {
        true
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Jwt {
    #[serde(default = "Jwt::secret_default")]
    pub secret: String,
    #[serde(default = "Jwt::expire_default")]
    pub expire: i64,
    #[serde(default = "Jwt::issuer_default")]
    pub issuer: String,
}

impl Default for Jwt {
    fn default() -> Self {
        Jwt {
            secret: Jwt::secret_default(),
            expire: Jwt::expire_default(),
            issuer: Jwt::issuer_default(),
        }
    }
}

impl Jwt {
    fn secret_default() -> String {
        "ng-gateway".into()
    }

    fn expire_default() -> i64 {
        3_600_000
    }

    fn issuer_default() -> String {
        "ng-gateway".into()
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Db {
    #[serde(default)]
    pub sqlite: Sqlite,
}

/// SQLite database type enum
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SqlType {
    #[default]
    Sqlite,
}

/// NGDbConfig is a trait that defines the necessary methods for database configuration.
/// It includes methods to get the database file path and connection URL for SQLite.
pub trait NGDbConfig: Send + Sync {
    /// Returns the type of SQL database.
    fn db_type(&self) -> SqlType;

    /// Returns the database file path.
    fn db_path(&self) -> String;

    /// Generates a URL for the database connection.
    fn to_url(&self) -> String;

    /// Returns the directory containing the database file.
    fn db_dir(&self) -> String;
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sqlite {
    #[serde(default = "Sqlite::path_default")]
    pub path: String,
    #[serde(default = "Sqlite::timeout_default")]
    pub timeout: u64,
    #[serde(default = "Sqlite::idle_timeout_default")]
    pub idle_timeout: u64,
    #[serde(default = "Sqlite::max_lifetime_default")]
    pub max_lifetime: u64,
    #[serde(default = "Sqlite::max_connections_default")]
    pub max_connections: u32,
    #[serde(default = "Sqlite::auto_create_default")]
    pub auto_create: bool,
}

impl Default for Sqlite {
    fn default() -> Self {
        Sqlite {
            path: Sqlite::path_default(),
            timeout: Sqlite::timeout_default(),
            idle_timeout: Sqlite::idle_timeout_default(),
            max_lifetime: Sqlite::max_lifetime_default(),
            max_connections: Sqlite::max_connections_default(),
            auto_create: Sqlite::auto_create_default(),
        }
    }
}

impl NGDbConfig for Sqlite {
    fn db_type(&self) -> SqlType {
        SqlType::Sqlite
    }

    fn db_path(&self) -> String {
        self.path.clone()
    }

    fn to_url(&self) -> String {
        if self.auto_create {
            // Use mode=rwc to automatically create file if it doesn't exist
            // r = read, w = write, c = create
            format!("sqlite:{}/{}?mode=rwc", DATA_DIR, self.path)
        } else {
            format!("sqlite:{}/{}", DATA_DIR, self.path)
        }
    }

    fn db_dir(&self) -> String {
        DATA_DIR.into()
    }
}

impl Sqlite {
    fn path_default() -> String {
        "ng-gateway.db".into()
    }

    fn timeout_default() -> u64 {
        5000
    }

    fn idle_timeout_default() -> u64 {
        5000
    }

    fn max_lifetime_default() -> u64 {
        5000
    }

    fn max_connections_default() -> u32 {
        100
    }

    fn auto_create_default() -> bool {
        true
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Cache {
    #[serde(default)]
    pub r#type: CacheType, // only "moka"
    #[serde(default = "Cache::prefix_default")]
    pub prefix: String,
    #[serde(default = "Cache::delimiter_default")]
    pub delimiter: String,
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            r#type: Default::default(),
            prefix: Cache::prefix_default(),
            delimiter: Cache::delimiter_default(),
        }
    }
}

impl Cache {
    fn prefix_default() -> String {
        "ng".into()
    }

    fn delimiter_default() -> String {
        ":".into()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CacheType {
    #[default]
    Moka,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Metrics {
    #[serde(default = "Metrics::enabled_default")]
    pub enabled: bool,
    #[serde(default = "Metrics::endpoint_default")]
    pub endpoint: String,
    #[serde(default = "Metrics::export_interval_default")]
    pub export_interval: u64,
    #[serde(default = "Metrics::service_name_default")]
    pub service_name: String,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            enabled: Metrics::enabled_default(),
            endpoint: Metrics::endpoint_default(),
            export_interval: Metrics::export_interval_default(),
            service_name: Metrics::service_name_default(),
        }
    }
}

impl Metrics {
    fn enabled_default() -> bool {
        false
    }

    fn endpoint_default() -> String {
        "http://localhost:4317".into()
    }

    fn export_interval_default() -> u64 {
        60000
    }

    fn service_name_default() -> String {
        "ng".into()
    }
}

/// Southward communication configuration
#[derive(Debug, Clone, Deserialize)]
pub struct Southward {
    /// API synchronous start wait timeout for driver connection (milliseconds)
    #[serde(default = "Southward::driver_sync_start_timeout_ms_default")]
    pub driver_sync_start_timeout_ms: u64,
}

impl Default for Southward {
    fn default() -> Self {
        Self {
            driver_sync_start_timeout_ms: Southward::driver_sync_start_timeout_ms_default(),
        }
    }
}

impl Southward {
    fn driver_sync_start_timeout_ms_default() -> u64 {
        5000
    }
}

/// Northward communication configuration
#[derive(Debug, Clone, Deserialize)]
pub struct Northward {
    /// Internal bounded queue capacity for northward manager
    #[serde(default = "Northward::queue_capacity_default")]
    pub queue_capacity: usize,
    /// API synchronous start wait timeout for northward app (milliseconds)
    #[serde(default = "Northward::app_sync_start_timeout_ms_default")]
    pub app_sync_start_timeout_ms: u64,
    /// Cache TTL for device change detection cache (in milliseconds)
    #[serde(default = "Northward::cache_ttl_ms_default")]
    pub cache_ttl_ms: u64,
}

impl Default for Northward {
    fn default() -> Self {
        Self {
            queue_capacity: Northward::queue_capacity_default(),
            app_sync_start_timeout_ms: Northward::app_sync_start_timeout_ms_default(),
            cache_ttl_ms: Northward::cache_ttl_ms_default(),
        }
    }
}

impl Northward {
    fn queue_capacity_default() -> usize {
        10000
    }

    fn app_sync_start_timeout_ms_default() -> u64 {
        5000
    }

    fn cache_ttl_ms_default() -> u64 {
        3600000
    }
}
