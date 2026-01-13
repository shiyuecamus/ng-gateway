use std::env;

fn main() {
    // SDK API version: bump when breaking changes to plugin ABI occur
    // Priority:
    // 1) Respect explicit env `NG_SDK_API_VERSION` if provided by CI/workspace
    // 2) Otherwise, default to Cargo package major version to reduce config burden
    let api_version = env::var("NG_SDK_API_VERSION").unwrap_or_else(|_| {
        env::var("CARGO_PKG_VERSION_MAJOR").unwrap_or_else(|_| "1".to_string())
    });
    println!("cargo:rustc-env=NG_SDK_API_VERSION={}", api_version);

    // SDK crate version (SemVer)
    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    println!("cargo:rustc-env=NG_SDK_VERSION={}", pkg_version);
}
