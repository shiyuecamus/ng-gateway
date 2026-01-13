use crate::{
    ensure_current_platform_from_path, inspect_binary,
    sdk::{sdk_api_version, SDK_VERSION},
    BinaryArch, BinaryOsType, NorthwardError, PluginConfigSchemas,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::ffi::CStr;
use std::os::raw::{c_char, c_uchar};
use std::{path::Path, sync::Arc};

use super::PluginFactory;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use libloading::{Library, Symbol};

/// Northward registry for managing all available northward plugin factories, keyed by adapter_type
pub type NorthwardRegistry = Arc<DashMap<i32, Arc<dyn PluginFactory + Send + Sync>>>;

/// Summary information about a northward library discovered via FFI symbols
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
    /// File size in bytes of the probed library
    pub size: i64,
    /// SHA-256 checksum (hex, lowercase)
    pub checksum: String,
    /// Detected OS type from the binary header
    pub os_type: BinaryOsType,
    /// Detected CPU architecture from the binary header
    pub os_arch: BinaryArch,
}

/// Dynamic northward loader for loading custom northward plugins from shared libraries
#[derive(Clone)]
pub struct NorthwardLoader {
    /// Registry of loaded northward factories
    registry: NorthwardRegistry,
    /// Hold library handles to ensure symbol and factory lifetimes are valid
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    libraries: Arc<DashMap<i32, Arc<Library>>>,
}

impl NorthwardLoader {
    /// Create a new northward loader with the given registry
    pub fn new(registry: NorthwardRegistry) -> Self {
        Self {
            registry,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            libraries: Arc::new(DashMap::new()),
        }
    }

    /// Register a northward factory directly (for built-in plugins), keyed by adapter_type
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

    /// Unregister a northward factory and release its library handle (if any), by adapter_type
    pub async fn unregister(&self, id: i32) {
        let _ = self.registry.remove(&id);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let _ = self.libraries.remove(&id);
        }
        tracing::info!("Unregistered northward factory: id={}", id);
    }

    /// Load and register northward plugins from provided (id, absolute path) pairs.
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

    /// Load a single northward library and register its factory into the registry.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub async fn load_library(
        &self,
        path: &Path,
        id: i32,
    ) -> Result<NorthwardProbeInfo, NorthwardError> {
        tracing::info!("Loading northward library: id={} {}", id, path.display());

        let path_buf = path.to_path_buf();
        let (library, probe_info, factory_box) = tokio::task::spawn_blocking(move || {
            // Load the shared library
            let library = unsafe { Library::new(&path_buf) }.map_err(|e| {
                NorthwardError::LoadError(format!(
                    "Failed to load library {}: {e}",
                    path_buf.display()
                ))
            })?;

            // Extract probe info first (validation + metadata)
            let probe_info = extract_probe_info(&library, &path_buf)?;

            // Look for the northward factory creation function only when registering
            let create_factory_fn: Symbol<unsafe extern "C" fn() -> *mut dyn PluginFactory> =
                unsafe { library.get(b"create_plugin_factory") }.map_err(|e| {
                    NorthwardError::LoadError(format!(
                        "Failed to find 'create_plugin_factory' symbol in {}: {e}",
                        path_buf.display()
                    ))
                })?;

            // Reconstruct Box from raw pointer
            let factory_ptr = unsafe { create_factory_fn() };
            if factory_ptr.is_null() {
                return Err(NorthwardError::LoadError(format!(
                    "Factory pointer was null from {}",
                    path_buf.display()
                )));
            }
            let factory_box: Box<dyn PluginFactory> = unsafe { Box::from_raw(factory_ptr) };

            // Try to configure plugin-specific tracing
            let init_tracing_fn: Symbol<unsafe extern "C" fn(bool)> =
                unsafe { library.get(b"ng_plugin_init_tracing") }.map_err(|e| {
                    NorthwardError::LoadError(format!(
                        "Failed to find 'ng_plugin_init_tracing' symbol in {}: {e}",
                        path_buf.display()
                    ))
                })?;

            unsafe { init_tracing_fn(cfg!(debug_assertions)) }

            Ok((library, probe_info, factory_box))
        })
        .await
        .map_err(|e| NorthwardError::LoadError(format!("Join error: {}", e)))??;

        let factory: Arc<dyn PluginFactory> = Arc::from(factory_box);

        // Register the factory by adapter_type
        self.register_factory(id, factory).await?;

        // Hold the library handle to ensure symbol and factory lifetimes remain valid
        self.libraries.insert(id, Arc::new(library));

        tracing::info!(
            "Successfully loaded northward plugin: id={} name={}",
            id,
            probe_info.name
        );

        Ok(probe_info)
    }

    /// Stub implementation for unsupported platforms
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    async fn load_library(
        &self,
        _path: &Path,
        _adapter_type: &str,
    ) -> Result<NorthwardProbeInfo, NorthwardError> {
        Err(NorthwardError::LoadError(
            "Dynamic northward loading not supported on this platform".to_string(),
        ))
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn extract_probe_info(
    library: &Library,
    path: &Path,
) -> Result<NorthwardProbeInfo, NorthwardError> {
    // Gate: api version, sdk version, adapter type, version, metadata json
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

    // Validate API version
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

    // Obtain metadata bytes pointer+len and copy immediately
    let mut ptr: *const c_uchar = std::ptr::null();
    let mut len: usize = 0;
    unsafe { metadata_ptr_fn(&mut ptr, &mut len) };
    if ptr.is_null() || len == 0 {
        return Err(NorthwardError::LoadError(format!(
            "Failed to obtain northward metadata json from {} (ptr={:?} len={}); plugin panic or metadata serialization failed",
            path.display()
            , ptr, len
        )));
    }
    let json_slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    let metadata: PluginConfigSchemas = serde_json::from_slice(json_slice).map_err(|e| {
        NorthwardError::LoadError(format!(
            "Failed to parse northward metadata json in {}: {e}",
            path.display()
        ))
    })?;

    // Obtain file size using synchronous metadata to avoid introducing async in loader
    let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);

    // Read bytes once to compute checksum and inspect OS/Arch
    let bytes = std::fs::read(path).map_err(|e| {
        NorthwardError::LoadError(format!("Failed to read library {}: {e}", path.display()))
    })?;
    let info = inspect_binary(&bytes);
    let os_type = info.os_type;
    let os_arch = info.arch;

    // Compute sha256
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
        os_type,
        os_arch,
    })
}

/// Probe a single northward library to extract versioning and metadata info without registering it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn probe_north_library(path: &Path) -> Result<NorthwardProbeInfo, NorthwardError> {
    // Early platform validation to avoid dlopen/symbol errors on mismatched binaries.
    ensure_current_platform_from_path(path)
        .map_err(|e| NorthwardError::LoadError(e.to_string()))?;

    let library = unsafe { Library::new(path) }.map_err(|e| {
        NorthwardError::LoadError(format!("Failed to load library {}: {e}", path.display()))
    })?;
    extract_probe_info(&library, path)
}

/// Stub probe for unsupported platforms
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn probe_north_library(_path: &Path) -> Result<NorthwardProbeInfo, NorthwardError> {
    Err(NorthwardError::LoadError(
        "Dynamic northward probing not supported on this platform".to_string(),
    ))
}

/// Discover northward libraries in a directory by file extension and probe them.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn discover_north_libraries_in_dir(dir: &Path) -> Vec<(String, NorthwardProbeInfo)> {
    let mut out = Vec::new();
    let expected_ext = BinaryOsType::current().expected_driver_ext().to_string();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match path.extension().and_then(|s| s.to_str()) {
            Some(ext) if ext == expected_ext => {
                if let Ok(info) = probe_north_library(&path) {
                    out.push((path.to_string_lossy().into_owned(), info));
                }
            }
            _ => {}
        }
    }
    out
}

/// Stubbed discovery for unsupported platforms
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn discover_north_libraries_in_dir(_dir: &Path) -> Vec<(String, NorthwardProbeInfo)> {
    Vec::new()
}
