//! Network management module.
//!
//! Provides cross-platform network interface enumeration, Wi-Fi scanning/connection,
//! AP hotspot management, and DNS configuration.
//!
//! # Architecture
//! - [`PlatformNetworkManager`] trait abstracts platform differences.
//! - [`NetworkService`] is the high-level façade consumed by REST API handlers.
//! - [`ap_config`] renders hostapd / dnsmasq configuration files.
//! - [`ap_manager`] controls AP systemd services via D-Bus.
//! - [`capability`] detects wireless hardware capabilities.
//! - Platform implementations live under [`platform`].

#[cfg(target_os = "linux")]
pub mod ap_config;
#[cfg(target_os = "linux")]
pub mod ap_manager;
pub mod capability;
pub mod platform;
pub mod service;
pub mod types;

pub use platform::PlatformNetworkManager;
pub use service::NetworkService;
