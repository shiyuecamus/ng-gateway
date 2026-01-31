//! Host-side dynamic northward plugin loader (core-owned).
//!
//! This module centralizes the host process logic for loading northward plugin `cdylib`s.
//! It is intentionally placed in `ng-gateway-core` (not SDK) to keep host responsibilities
//! out of the SDK and align with the driver loader design.

use dashmap::DashMap;
use libloading::{Library, Symbol};
use ng_gateway_common::log::{
    control::{self as log_control},
    plugin::{create_sink, ensure_ingest_started, HostPluginLogSinkHandle},
};
use ng_gateway_models::domain::prelude::LogLevel;
use ng_gateway_sdk::{
    ensure_current_platform_from_path, inspect_binary,
    log::LogSinkV1,
    northward::PluginFactory,
    sdk::{sdk_api_version, SDK_VERSION},
    BinaryArch, BinaryOsType, NorthwardError, PluginConfigSchemas,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CStr,
    os::raw::{c_char, c_uchar},
    path::Path,
    sync::Arc,
};

/// Northward registry for managing all available plugin factories, keyed by plugin id.
pub type NorthwardRegistry = Arc<DashMap<i32, Arc<dyn PluginFactory + Send + Sync>>>;

/// Exported function pointer for setting plugin max log level.
///
/// Level mapping:
/// - 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
type PluginSetMaxLevelFn = unsafe extern "C" fn(u8) -> u32;

/// Summary information about a northward library discovered via FFI symbols.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NorthwardProbeInfo {
    pub plugin_type: String,
    pub name: String,
    pub description: Option<String>,
    pub version: String,
    pub api_version: u32,
    pub sdk_version: String,
    pub metadata: PluginConfigSchemas,
    /// File size in bytes of the probed library.
    pub size: i64,
    /// SHA-256 checksum (hex, lowercase).
    pub checksum: String,
    /// Detected OS type from the binary header.
    pub os_type: BinaryOsType,
    /// Detected CPU architecture from the binary header.
    pub os_arch: BinaryArch,
}

/// Dynamic northward loader for loading custom plugins from shared libraries.
#[derive(Clone)]
pub struct NorthwardLoader {
    registry: NorthwardRegistry,
    libraries: Arc<DashMap<i32, Arc<Library>>>,
    /// Keep host sink contexts alive for FFI callbacks.
    log_sinks: Arc<DashMap<i32, HostPluginLogSinkHandle>>,
    /// Optional exported setter for dynamic plugin log level control.
    max_level_setters: Arc<DashMap<i32, PluginSetMaxLevelFn>>,
}

impl NorthwardLoader {
    /// Create a new northward loader with the given registry.
    pub fn new(registry: NorthwardRegistry) -> Self {
        Self {
            registry,
            libraries: Arc::new(DashMap::new()),
            log_sinks: Arc::new(DashMap::new()),
            max_level_setters: Arc::new(DashMap::new()),
        }
    }

    /// Register a factory directly (for built-in plugins).
    pub async fn register_factory(
        &self,
        id: i32,
        factory: Arc<dyn PluginFactory + Send + Sync>,
    ) -> Result<(), NorthwardError> {
        if self.registry.contains_key(&id) {
            return Err(NorthwardError::LoadError(format!(
                "Northward id '{}' already registered",
                id
            )));
        }
        self.registry.insert(id, factory);
        tracing::info!("Registered northward factory: id={}", id);
        Ok(())
    }

    /// Unregister a factory and release its library handle (if any).
    pub async fn unregister(&self, id: i32) {
        let _ = self.registry.remove(&id);
        let _ = self.libraries.remove(&id);
        let _ = self.log_sinks.remove(&id);
        let _ = self.max_level_setters.remove(&id);
        tracing::info!("Unregistered northward factory: id={}", id);
    }

    /// Best-effort set plugin max log level via exported symbol.
    ///
    /// Returns `true` if the plugin exported the symbol and returned success.
    pub fn set_max_level(&self, id: i32, level: u8) -> bool {
        let Some(f) = self.max_level_setters.get(&id) else {
            return false;
        };
        let rc = unsafe { (*f)(level) };
        rc == 0
    }

    /// Best-effort set max log level for all loaded plugins.
    ///
    /// Returns number of plugins updated successfully.
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

    /// Load and register plugins from provided (id, absolute path) pairs.
    pub async fn load_all(&self, plugins: &[(i32, String)]) {
        let mut set = tokio::task::JoinSet::new();
        for (id, p) in plugins {
            let id = *id;
            let p = p.clone();
            let loader = self.clone();
            set.spawn(async move {
                let path = Path::new(&p);
                if let Err(e) = loader.load_library(path, id).await {
                    tracing::warn!(error=%e, "Failed to load northward library id={} path={}", id, p);
                }
            });
        }
        while set.join_next().await.is_some() {}
    }

    /// Load a single plugin library and register its factory into the registry.
    pub async fn load_library(
        &self,
        path: &Path,
        id: i32,
    ) -> Result<NorthwardProbeInfo, NorthwardError> {
        tracing::info!("Loading northward library: id={} {}", id, path.display());

        // Early platform validation to avoid dlopen/symbol errors on mismatched binaries.
        ensure_current_platform_from_path(path)
            .map_err(|e| NorthwardError::LoadError(e.to_string()))?;

        // Ensure host ingest loop is running (idempotent).
        ensure_ingest_started();

        let path_buf = path.to_path_buf();
        let (library, probe_info, factory_box, log_sink_handle, set_max_level_fn) =
            tokio::task::spawn_blocking(move || {
                let library = unsafe { Library::new(&path_buf) }.map_err(|e| {
                    NorthwardError::LoadError(format!(
                        "Failed to load library {}: {e}",
                        path_buf.display()
                    ))
                })?;

                // Register log sink + init tracing BEFORE calling any other exported symbols.
                let log_sink_handle = match unsafe {
                    library.get::<unsafe extern "C" fn(LogSinkV1) -> u32>(b"ng_plugin_set_log_sink")
                } {
                    Ok(set_sink_fn) => {
                        let handle = create_sink(id, "unknown".into());
                        let rc = unsafe { set_sink_fn(handle.sink()) };
                        if rc != 0 {
                            tracing::warn!(
                                plugin_id = id,
                                rc = rc,
                                "Plugin did not accept log sink registration"
                            );
                        }
                        Some(handle)
                    }
                    Err(_) => None,
                };

                // Initialize tracing in the plugin (bridge-only).
                let init_tracing_fn: Symbol<unsafe extern "C" fn(bool)> =
                    unsafe { library.get(b"ng_plugin_init_tracing") }.map_err(|e| {
                        NorthwardError::LoadError(format!(
                            "Failed to find 'ng_plugin_init_tracing' symbol in {}: {e}",
                            path_buf.display()
                        ))
                    })?;
                unsafe { init_tracing_fn(cfg!(debug_assertions)) };

                // Optional: dynamic log level control.
                let set_max_level_fn: Option<PluginSetMaxLevelFn> = match unsafe {
                    library.get::<unsafe extern "C" fn(u8) -> u32>(b"ng_plugin_set_max_level")
                } {
                    Ok(f) => Some(*f),
                    Err(_) => None,
                };

                let probe_info = extract_probe_info(&library, &path_buf)?;
                if let Some(ref h) = log_sink_handle {
                    h.set_plugin_type(probe_info.plugin_type.clone());
                }

                let create_factory_fn: Symbol<unsafe extern "C" fn() -> *mut dyn PluginFactory> =
                    unsafe { library.get(b"create_plugin_factory") }.map_err(|e| {
                        NorthwardError::LoadError(format!(
                            "Failed to find 'create_plugin_factory' symbol in {}: {e}",
                            path_buf.display()
                        ))
                    })?;

                let factory_ptr = unsafe { create_factory_fn() };
                if factory_ptr.is_null() {
                    return Err(NorthwardError::LoadError(format!(
                        "Factory pointer was null from {}",
                        path_buf.display()
                    )));
                }
                let factory_box: Box<dyn PluginFactory> = unsafe { Box::from_raw(factory_ptr) };

                Ok((
                    library,
                    probe_info,
                    factory_box,
                    log_sink_handle,
                    set_max_level_fn,
                ))
            })
            .await
            .map_err(|e| NorthwardError::LoadError(format!("Join error: {}", e)))??;

        let factory: Arc<dyn PluginFactory> = Arc::from(factory_box);
        self.register_factory(id, factory).await?;
        self.libraries.insert(id, Arc::new(library));
        if let Some(h) = log_sink_handle {
            self.log_sinks.insert(id, h);
        }
        if let Some(f) = set_max_level_fn {
            self.max_level_setters.insert(id, f);

            // Best-effort: align plugin max level to current effective global level.
            if let Some(rt) = log_control::global() {
                let desired: u8 = rt.overrides().effective_global_level().into();
                let _ = self.set_max_level(id, desired);
            } else {
                let _ = self.set_max_level(id, u8::from(LogLevel::Info));
            }
        }

        tracing::info!(
            "Successfully loaded northward plugin: id={} name={}",
            id,
            probe_info.name
        );

        Ok(probe_info)
    }
}

#[inline]
fn read_cstr(ptr: *const c_char, label: &str, path: &Path) -> Result<String, NorthwardError> {
    if ptr.is_null() {
        return Err(NorthwardError::LoadError(format!(
            "Northward symbol '{}' returned NULL in {} (plugin panic or invalid ABI)",
            label,
            path.display()
        )));
    }
    Ok(unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned())
}

fn extract_probe_info(
    library: &Library,
    path: &Path,
) -> Result<NorthwardProbeInfo, NorthwardError> {
    let api_version_fn: Symbol<unsafe fn() -> u32> =
        unsafe { library.get(b"ng_plugin_api_version") }.map_err(|e| {
            NorthwardError::LoadError(format!(
                "Missing 'ng_plugin_api_version' in {}: {e}",
                path.display()
            ))
        })?;

    let sdk_version_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_plugin_sdk_version") }.map_err(|e| {
            NorthwardError::LoadError(format!(
                "Missing 'ng_plugin_sdk_version' in {}: {e}",
                path.display()
            ))
        })?;

    let plugin_type_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_plugin_type") }.map_err(|e| {
            NorthwardError::LoadError(format!(
                "Missing 'ng_plugin_type' in {}: {e}",
                path.display()
            ))
        })?;

    let name_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_plugin_name") }.map_err(|e| {
            NorthwardError::LoadError(format!(
                "Missing 'ng_plugin_name' in {}: {e}",
                path.display()
            ))
        })?;

    let description_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_plugin_description") }.map_err(|e| {
            NorthwardError::LoadError(format!(
                "Missing 'ng_plugin_description' in {}: {e}",
                path.display()
            ))
        })?;

    let version_fn: Symbol<unsafe extern "C" fn() -> *const c_char> =
        unsafe { library.get(b"ng_plugin_version") }.map_err(|e| {
            NorthwardError::LoadError(format!(
                "Missing 'ng_plugin_version' in {}: {e}",
                path.display()
            ))
        })?;

    let metadata_ptr_fn: Symbol<unsafe extern "C" fn(*mut *const c_uchar, *mut usize)> =
        unsafe { library.get(b"ng_plugin_metadata_json_ptr") }.map_err(|e| {
            NorthwardError::LoadError(format!(
                "Missing 'ng_plugin_metadata_json_ptr' in {}: {e}",
                path.display()
            ))
        })?;

    let api_version = unsafe { api_version_fn() };
    let host_api_version = sdk_api_version();
    if api_version != host_api_version {
        return Err(NorthwardError::LoadError(format!(
            "API version mismatch: plugin={} host={}",
            api_version, host_api_version
        )));
    }

    let plugin_sdk_version_str =
        read_cstr(unsafe { sdk_version_fn() }, "ng_plugin_sdk_version", path)?;
    let host_sdk_version = SDK_VERSION;
    if plugin_sdk_version_str != host_sdk_version {
        tracing::warn!(
            "SDK version mismatch: plugin={} host={}; proceeding due to non-strict policy",
            plugin_sdk_version_str,
            host_sdk_version
        );
    }

    let plugin_type = read_cstr(unsafe { plugin_type_fn() }, "ng_plugin_type", path)?;
    let name = read_cstr(unsafe { name_fn() }, "ng_plugin_name", path)?;
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
    let version = read_cstr(unsafe { version_fn() }, "ng_plugin_version", path)?;

    let mut ptr: *const c_uchar = std::ptr::null();
    let mut len: usize = 0;
    unsafe { metadata_ptr_fn(&mut ptr, &mut len) };
    if ptr.is_null() || len == 0 {
        return Err(NorthwardError::LoadError(format!(
            "Failed to obtain northward metadata json from {} (ptr={:?} len={}); plugin panic or metadata serialization failed",
            path.display(),
            ptr,
            len
        )));
    }
    let json_slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    let metadata: PluginConfigSchemas = serde_json::from_slice(json_slice).map_err(|e| {
        NorthwardError::LoadError(format!(
            "Failed to parse northward metadata json in {}: {e}",
            path.display()
        ))
    })?;

    let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
    let bytes = std::fs::read(path).map_err(|e| {
        NorthwardError::LoadError(format!("Failed to read library {}: {e}", path.display()))
    })?;
    let info = inspect_binary(&bytes);

    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(&bytes);
    let checksum = hex::encode(hasher.finalize());

    Ok(NorthwardProbeInfo {
        plugin_type,
        name,
        description,
        version,
        api_version,
        sdk_version: plugin_sdk_version_str.to_string(),
        metadata,
        size,
        checksum,
        os_type: info.os_type,
        os_arch: info.arch,
    })
}
