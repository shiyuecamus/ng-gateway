//! Northward plugin probing utilities (host-side).
//!
//! This module provides lightweight "probe" functions for inspecting a `cdylib` plugin library
//! without registering or running it in the gateway runtime.
//!
//! # Notes
//! The actual host-side dynamic loader lives in `ng-gateway-core`.

use crate::{BinaryArch, BinaryOsType, NorthwardError, PluginConfigSchemas};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::{
    ensure_current_platform_from_path, inspect_binary,
    sdk::{sdk_api_version, SDK_VERSION},
};
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    ffi::CStr,
    os::raw::{c_char, c_uchar},
};
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use libloading::{Library, Symbol};

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

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

/// Probe a single northward library to extract versioning and metadata info without registering it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn probe_north_library(path: &Path) -> Result<NorthwardProbeInfo, NorthwardError> {
    ensure_current_platform_from_path(path)
        .map_err(|e| NorthwardError::LoadError(e.to_string()))?;
    let library = unsafe { Library::new(path) }.map_err(|e| {
        NorthwardError::LoadError(format!("Failed to load library {}: {e}", path.display()))
    })?;
    extract_probe_info(&library, path)
}

/// Stub probe for unsupported platforms.
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

/// Stubbed discovery for unsupported platforms.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn discover_north_libraries_in_dir(_dir: &Path) -> Vec<(String, NorthwardProbeInfo)> {
    Vec::new()
}
