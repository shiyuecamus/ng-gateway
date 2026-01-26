//! Dynamic driver loader for `cdylib` southward drivers (host side).
//!
//! This is a host-owned implementation (lives in `ng-gateway-core`) so it can integrate with
//! the gateway's unified logging subsystem (`ng-gateway-common::log`) and runtime policies.

use dashmap::DashMap;
use libloading::{Library, Symbol};
use ng_gateway_common::log::{self, realtime::hub::LogLevel};
use ng_gateway_sdk::{
    ensure_current_platform_from_path, inspect_binary,
    sdk::{sdk_api_version, SDK_VERSION},
    BinaryArch, BinaryOsType, DriverError, DriverFactory, DriverResult, DriverSchemas,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CStr,
    os::raw::{c_char, c_uchar},
    path::Path,
    sync::Arc,
};

/// Driver registry for managing all available drivers, keyed by driver_id.
pub type DriverRegistry = Arc<DashMap<i32, Arc<dyn DriverFactory + Send + Sync>>>;

/// Exported function pointer for setting driver max log level.
///
/// Level mapping:
/// - 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
type DriverSetMaxLevelFn = unsafe extern "C" fn(u8) -> u32;

/// Summary information about a driver library discovered via FFI symbols.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverProbeInfo {
    pub driver_type: String,
    pub name: String,
    pub description: Option<String>,
    pub version: String,
    pub api_version: u32,
    pub sdk_version: String,
    pub metadata: DriverSchemas,
    /// File size in bytes of the probed driver library
    pub size: i64,
    /// SHA-256 checksum (hex, lowercase)
    pub checksum: String,
    /// Detected OS type from the binary header
    pub os_type: BinaryOsType,
    /// Detected CPU architecture from the binary header
    pub os_arch: BinaryArch,
}

/// Dynamic driver loader for loading custom drivers from shared libraries.
#[derive(Clone)]
pub struct DriverLoader {
    registry: DriverRegistry,
    libraries: Arc<DashMap<i32, Arc<Library>>>,
    /// Keep host sink contexts alive for FFI callbacks.
    log_sinks: Arc<DashMap<i32, log::driver::HostLogSinkHandle>>,
    /// Optional exported setter for dynamic driver log level control.
    max_level_setters: Arc<DashMap<i32, DriverSetMaxLevelFn>>,
}

impl DriverLoader {
    /// Create a new driver loader with the given registry.
    pub fn new(registry: DriverRegistry) -> Self {
        Self {
            registry,
            libraries: Arc::new(DashMap::new()),
            log_sinks: Arc::new(DashMap::new()),
            max_level_setters: Arc::new(DashMap::new()),
        }
    }

    /// Register a driver factory directly (for built-in drivers), keyed by driver_id.
    pub async fn register_factory(
        &self,
        driver_id: i32,
        factory: Arc<dyn DriverFactory + Send + Sync>,
    ) -> DriverResult<()> {
        if self.registry.contains_key(&driver_id) {
            return Err(DriverError::LoadError(format!(
                "Driver id '{driver_id}' already registered"
            )));
        }
        self.registry.insert(driver_id, factory);
        tracing::info!("Registered driver factory: id={}", driver_id);
        Ok(())
    }

    /// Unregister a driver factory and release its library handle (if any), by driver_id.
    pub async fn unregister(&self, driver_id: i32) {
        let _ = self.registry.remove(&driver_id);
        let _ = self.libraries.remove(&driver_id);
        let _ = self.log_sinks.remove(&driver_id);
        let _ = self.max_level_setters.remove(&driver_id);
        tracing::info!("Unregistered driver factory: id={}", driver_id);
    }

    /// Best-effort set driver max log level via exported symbol.
    ///
    /// Returns `true` if the driver exported the symbol and returned success.
    pub fn set_max_level(&self, driver_id: i32, level: u8) -> bool {
        let Some(f) = self.max_level_setters.get(&driver_id) else {
            return false;
        };
        let rc = unsafe { (*f)(level) };
        rc == 0
    }

    /// Best-effort set max log level for all loaded drivers.
    ///
    /// Returns number of drivers updated successfully.
    pub fn set_max_level_all(&self, level: u8) -> usize {
        let mut ok: usize = 0;
        for e in self.max_level_setters.iter() {
            let rc = unsafe { (*e.value())(level) };
            if rc == 0 {
                ok += 1;
            }
        }
        ok
    }

    /// Load and register drivers from provided (driver_id, absolute path) pairs.
    pub async fn load_all(&self, drivers: &[(i32, String)]) {
        let mut set = tokio::task::JoinSet::new();
        for (driver_id, p) in drivers {
            let driver_id = *driver_id;
            let p = p.clone();
            let loader = self.clone();
            set.spawn(async move {
                let path = Path::new(&p);
                if let Err(e) = loader.load_driver_library(path, driver_id).await {
                    tracing::warn!(error=%e, "Failed to load driver library id={} path={}", driver_id, p);
                }
            });
        }
        while set.join_next().await.is_some() {}
    }

    /// Load a single driver library and register it into the registry.
    pub async fn load_driver_library(
        &self,
        path: &Path,
        driver_id: i32,
    ) -> DriverResult<DriverProbeInfo> {
        tracing::info!(
            "Loading driver library: id={} {}",
            driver_id,
            path.display()
        );

        // Fast preflight to avoid dlopen errors on mismatched binaries.
        ensure_current_platform_from_path(path)?;

        // Ensure host ingest loop is running (idempotent).
        log::driver::ensure_ingest_started();

        let path_buf = path.to_path_buf();
        let (library, probe_info, factory_box, log_sink_handle, set_max_level_fn) =
            tokio::task::spawn_blocking(move || {
                let library = unsafe { Library::new(&path_buf) }.map_err(|e| {
                    DriverError::LoadError(format!(
                        "Failed to load library {}: {e}",
                        path_buf.display()
                    ))
                })?;

                // Register log sink + init tracing BEFORE calling any other exported symbols.
                let log_sink_handle = match unsafe {
                    library.get::<unsafe extern "C" fn(ng_gateway_sdk::log::LogSinkV1) -> u32>(
                        b"ng_driver_set_log_sink",
                    )
                } {
                    Ok(set_sink_fn) => {
                        let handle = log::driver::create_sink(driver_id, "unknown".into());
                        let rc = unsafe { set_sink_fn(handle.sink()) };
                        if rc != 0 {
                            tracing::warn!(
                                driver_id = driver_id,
                                rc = rc,
                                "Driver did not accept log sink registration"
                            );
                        }
                        Some(handle)
                    }
                    Err(_) => None,
                };

                // Initialize tracing in the plugin (bridge-only).
                let init_tracing_fn: Symbol<unsafe extern "C" fn(bool)> =
                    unsafe { library.get(b"ng_driver_init_tracing") }.map_err(|e| {
                        DriverError::LoadError(format!(
                            "Failed to find 'ng_driver_init_tracing' symbol in {}: {e}",
                            path_buf.display()
                        ))
                    })?;
                unsafe { init_tracing_fn(cfg!(debug_assertions)) };

                // Optional: dynamic log level control.
                let set_max_level_fn: Option<DriverSetMaxLevelFn> = match unsafe {
                    library.get::<unsafe extern "C" fn(u8) -> u32>(b"ng_driver_set_max_level")
                } {
                    Ok(f) => Some(*f),
                    Err(_) => None,
                };

                // Extract probe info (validation + metadata).
                let probe_info = extract_probe_info(&library, &path_buf)?;
                if let Some(ref h) = log_sink_handle {
                    h.set_driver_type(probe_info.driver_type.clone());
                }

                // Look for the driver factory creation function.
                let create_factory_fn: Symbol<unsafe extern "C" fn() -> *mut dyn DriverFactory> =
                    unsafe { library.get(b"create_driver_factory") }.map_err(|e| {
                        DriverError::LoadError(format!(
                            "Failed to find 'create_driver_factory' symbol in {}: {e}",
                            path_buf.display()
                        ))
                    })?;

                let factory_ptr = unsafe { create_factory_fn() };
                if factory_ptr.is_null() {
                    return Err(DriverError::LoadError(format!(
                        "Factory pointer was null from {}",
                        path_buf.display()
                    )));
                }
                let factory_box: Box<dyn DriverFactory> = unsafe { Box::from_raw(factory_ptr) };

                Ok((
                    library,
                    probe_info,
                    factory_box,
                    log_sink_handle,
                    set_max_level_fn,
                ))
            })
            .await
            .map_err(|e| DriverError::LoadError(format!("Join error: {}", e)))??;

        let factory: Arc<dyn DriverFactory> = Arc::from(factory_box);
        self.register_factory(driver_id, factory).await?;
        self.libraries.insert(driver_id, Arc::new(library));
        if let Some(h) = log_sink_handle {
            self.log_sinks.insert(driver_id, h);
        }
        if let Some(f) = set_max_level_fn {
            self.max_level_setters.insert(driver_id, f);

            // Best-effort: align driver max level to current effective global level.
            if let Some(rt) = log::runtime::global() {
                let desired: u8 = rt.overrides().effective_global_level() as u8;
                let _ = self.set_max_level(driver_id, desired);
            } else {
                let _ = self.set_max_level(driver_id, LogLevel::Info as u8);
            }
        }

        tracing::info!(
            "Successfully loaded driver: id={} type={}",
            driver_id,
            probe_info.driver_type
        );

        Ok(probe_info)
    }
}

#[inline]
fn read_cstr(ptr: *const c_char, label: &str, path: &Path) -> DriverResult<String> {
    if ptr.is_null() {
        return Err(DriverError::LoadError(format!(
            "Driver symbol '{}' returned NULL in {} (plugin panic or invalid ABI)",
            label,
            path.display()
        )));
    }
    Ok(unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned())
}

fn extract_probe_info(library: &Library, path: &Path) -> DriverResult<DriverProbeInfo> {
    let api_version_fn: Symbol<unsafe fn() -> u32> =
        unsafe { library.get(b"ng_driver_api_version") }.map_err(|e| {
            DriverError::LoadError(format!(
                "Missing 'ng_driver_api_version' in {}: {e}",
                path.display()
            ))
        })?;

    let sdk_version_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_driver_sdk_version") }.map_err(|e| {
            DriverError::LoadError(format!(
                "Missing 'ng_driver_sdk_version' in {}: {e}",
                path.display()
            ))
        })?;

    let driver_type_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_driver_type") }.map_err(|e| {
            DriverError::LoadError(format!(
                "Missing 'ng_driver_type' in {}: {e}",
                path.display()
            ))
        })?;

    let name_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_driver_name") }.map_err(|e| {
            DriverError::LoadError(format!(
                "Missing 'ng_driver_name' in {}: {e}",
                path.display()
            ))
        })?;

    let description_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_driver_description") }.map_err(|e| {
            DriverError::LoadError(format!(
                "Missing 'ng_driver_description' in {}: {e}",
                path.display()
            ))
        })?;

    let version_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_driver_version") }.map_err(|e| {
            DriverError::LoadError(format!(
                "Missing 'ng_driver_version' in {}: {e}",
                path.display()
            ))
        })?;

    let metadata_ptr_fn: Symbol<unsafe extern "C" fn(*mut *const c_uchar, *mut usize)> =
        unsafe { library.get(b"ng_driver_metadata_json_ptr") }.map_err(|e| {
            DriverError::LoadError(format!(
                "Missing 'ng_driver_metadata_json_ptr' in {}: {e}",
                path.display()
            ))
        })?;

    let api_version = unsafe { api_version_fn() };
    let host_api_version = sdk_api_version();
    if api_version != host_api_version {
        return Err(DriverError::LoadError(format!(
            "API version mismatch: plugin={} host={}",
            api_version, host_api_version
        )));
    }

    let plugin_sdk_version_str =
        read_cstr(unsafe { sdk_version_fn() }, "ng_driver_sdk_version", path)?;
    let host_sdk_version = SDK_VERSION;
    if plugin_sdk_version_str != host_sdk_version {
        tracing::warn!(
            "SDK version mismatch: plugin={} host={}; proceeding due to non-strict policy",
            plugin_sdk_version_str,
            host_sdk_version
        );
    }

    let driver_type = read_cstr(unsafe { driver_type_fn() }, "ng_driver_type", path)?;
    let name = read_cstr(unsafe { name_fn() }, "ng_driver_name", path)?;
    let description_ptr = unsafe { description_fn() };
    let description = if description_ptr.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(description_ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    };
    let version = read_cstr(unsafe { version_fn() }, "ng_driver_version", path)?;

    let mut ptr: *const c_uchar = std::ptr::null();
    let mut len: usize = 0;
    unsafe { metadata_ptr_fn(&mut ptr, &mut len) };
    if ptr.is_null() || len == 0 {
        return Err(DriverError::LoadError(format!(
            "Failed to obtain metadata json from {} (ptr={:?} len={}); plugin panic or metadata serialization failed",
            path.display(),
            ptr,
            len
        )));
    }
    let json_slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    let driver_metadata: DriverSchemas = serde_json::from_slice(json_slice).map_err(|e| {
        DriverError::LoadError(format!(
            "Failed to parse driver metadata json in {}: {e}",
            path.display()
        ))
    })?;

    let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
    let bytes = std::fs::read(path).map_err(|e| {
        DriverError::LoadError(format!("Failed to read library {}: {e}", path.display()))
    })?;
    let info = inspect_binary(&bytes);
    let os_type = info.os_type;
    let os_arch = info.arch;

    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(&bytes);
    let checksum = hex::encode(hasher.finalize());

    Ok(DriverProbeInfo {
        driver_type,
        name,
        description,
        version,
        api_version,
        sdk_version: plugin_sdk_version_str.to_string(),
        metadata: driver_metadata,
        size,
        checksum,
        os_type,
        os_arch,
    })
}

/// Probe a single driver library to extract versioning and metadata info without registering it.
pub fn probe_driver_library(path: &Path) -> DriverResult<DriverProbeInfo> {
    ensure_current_platform_from_path(path)?;
    let library = unsafe { Library::new(path) }.map_err(|e| {
        DriverError::LoadError(format!("Failed to load library {}: {e}", path.display()))
    })?;
    extract_probe_info(&library, path)
}
