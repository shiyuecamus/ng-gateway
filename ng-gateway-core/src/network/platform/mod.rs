//! Platform-abstracted network management.
//!
//! Each target OS has its own implementation of [`PlatformNetworkManager`].
//! The correct implementation is selected at compile time via `cfg` attributes,
//! with runtime capability detection for feature availability.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::LinuxNetworkManager;
#[cfg(target_os = "macos")]
pub use macos::MacosNetworkManager;
#[cfg(target_os = "windows")]
pub use windows::WindowsNetworkManager;

use async_trait::async_trait;
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::domain::prelude::{
    ApStatus, ConfigureApRequest, ConfigureDnsRequest, ConfigureInterfaceRequest, DnsConfig,
    NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary, WifiAccessPoint,
    WifiBand, WifiConnectRequest, WifiSecurity, WifiStaStatus,
};

/// Platform-abstracted network management interface.
///
/// # Design
/// - **Linux**: implemented via NetworkManager D-Bus API (`zbus`).
/// - **macOS**: read-only fallback using `networksetup` / `system_profiler` commands.
/// - **Windows**: read-only fallback using `netsh` commands.
///
/// All methods are async to accommodate D-Bus I/O and subprocess execution.
///
/// # Error Handling
/// - Returns `NGResult<T>` with descriptive error context.
/// - Unsupported operations return `NetworkError::PlatformNotSupported`.
#[async_trait]
pub trait PlatformNetworkManager: Send + Sync {
    // ─── Discovery ───

    /// List all network interfaces with summary info.
    async fn list_interfaces(&self) -> NGResult<Vec<NetworkInterfaceSummary>>;

    /// Get detailed info for a specific interface by its system name.
    async fn get_interface(&self, name: &str) -> NGResult<NetworkInterfaceDetail>;

    /// Detect platform capabilities (supported features, STA+AP, etc.).
    async fn detect_capabilities(&self) -> NGResult<NetworkCapabilities>;

    // ─── Interface Configuration ───

    /// Configure IP settings for an interface (DHCP / static).
    async fn configure_interface(
        &self,
        name: &str,
        config: &ConfigureInterfaceRequest,
    ) -> NGResult<()>;

    // ─── Wi-Fi ───

    /// Request a Wi-Fi scan and return visible access points.
    async fn scan_wifi(&self, interface_name: Option<&str>) -> NGResult<Vec<WifiAccessPoint>>;

    /// Connect to a Wi-Fi network as STA.
    async fn connect_wifi(&self, request: &WifiConnectRequest) -> NGResult<WifiStaStatus>;

    /// Disconnect Wi-Fi STA on the given (or default) interface.
    async fn disconnect_wifi(&self, interface_name: Option<&str>) -> NGResult<()>;

    /// Get current Wi-Fi STA connection status.
    async fn wifi_sta_status(&self, interface_name: Option<&str>) -> NGResult<WifiStaStatus>;

    // ─── AP Hotspot ───

    /// Get current AP hotspot status.
    async fn ap_status(&self) -> NGResult<ApStatus>;

    /// Start the AP hotspot service stack.
    ///
    /// In `Exclusive` mode this will disconnect the current STA connection first.
    /// In `Concurrent` / `DedicatedCard` mode this starts AP alongside STA.
    async fn start_ap(&self) -> NGResult<ApStatus>;

    /// Stop the AP hotspot service stack.
    ///
    /// On Linux in `Exclusive` mode, if STA was disconnected when AP started, this will
    /// attempt to restore the previous Wi-Fi connection via NetworkManager's `ActivateConnection`.
    async fn stop_ap(&self) -> NGResult<ApStatus>;

    /// Update AP hotspot configuration.
    ///
    /// If the AP is running, restarts hostapd to apply changes.
    /// If the AP is stopped, only writes configuration files.
    async fn configure_ap(&self, config: &ConfigureApRequest) -> NGResult<ApStatus>;

    // ─── DNS ───

    /// Get current DNS configuration.
    async fn get_dns(&self) -> NGResult<DnsConfig>;

    /// Set DNS configuration.
    async fn configure_dns(&self, config: &ConfigureDnsRequest) -> NGResult<()>;
}

/// Create the platform-appropriate [`PlatformNetworkManager`] instance.
///
/// This is the single entry point for obtaining a network manager, selected at compile time.
pub async fn create_platform_manager() -> NGResult<Box<dyn PlatformNetworkManager>> {
    #[cfg(target_os = "linux")]
    {
        let manager = LinuxNetworkManager::new().await?;
        Ok(Box::new(manager))
    }
    #[cfg(target_os = "macos")]
    {
        let manager = MacosNetworkManager::new();
        Ok(Box::new(manager))
    }
    #[cfg(target_os = "windows")]
    {
        let manager = WindowsNetworkManager::new();
        Ok(Box::new(manager))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(ng_gateway_error::NGError::Error(
            "Network management is not supported on this platform".to_string(),
        ))
    }
}

/// Cross-platform Wi-Fi scan using the `wifi_scan` crate.
///
/// Uses native APIs on each platform:
/// - **macOS**: CoreWLAN framework
/// - **Windows**: win32-wlan (Native Wifi API)
/// - **Linux**: nl80211 / netlink (used as fallback when NM D-Bus is unavailable)
///
/// This runs a blocking scan in a `spawn_blocking` task to avoid blocking the async runtime.
pub async fn scan_wifi_native() -> NGResult<Vec<WifiAccessPoint>> {
    let result = tokio::task::spawn_blocking(wifi_scan::scan)
        .await
        .map_err(|e| NGError::Error(format!("Wi-Fi scan task failed: {e}")))?;

    match result {
        Ok(networks) => {
            tracing::debug!("Wi-Fi scan result: {:?}", networks);
            let mut aps: Vec<WifiAccessPoint> = networks
                .into_iter()
                .filter(|w| !w.is_hidden())
                .map(|w| {
                    let frequency = w.get_frequency();
                    let band = match frequency {
                        2400..=2500 => WifiBand::Band2_4Ghz,
                        5150..=5900 => WifiBand::Band5Ghz,
                        5925..=7125 => WifiBand::Band6Ghz,
                        _ => WifiBand::Unknown,
                    };

                    let security = if w.is_wpa3() {
                        WifiSecurity::Wpa3Sae
                    } else if w.is_wpa2() && w.is_enterprise() {
                        WifiSecurity::Wpa2Enterprise
                    } else if w.is_wpa2() {
                        WifiSecurity::Wpa2Psk
                    } else if w.is_enterprise() {
                        WifiSecurity::WpaEnterprise
                    } else if w.is_open() {
                        WifiSecurity::Open
                    } else {
                        WifiSecurity::Unknown
                    };

                    let signal_quality = rssi_to_quality(w.signal_level);

                    WifiAccessPoint {
                        ssid: w.ssid,
                        bssid: w.mac,
                        security,
                        band,
                        channel: w.channel,
                        frequency,
                        signal_dbm: w.signal_level,
                        signal_quality,
                        max_bitrate_kbps: None,
                        is_connected: false,
                    }
                })
                .collect();

            // Sort by signal descending, deduplicate by SSID.
            aps.sort_by(|a, b| b.signal_quality.cmp(&a.signal_quality));
            let mut seen = std::collections::HashSet::new();
            aps.retain(|ap| {
                if ap.ssid.is_empty() {
                    return false;
                }
                seen.insert(ap.ssid.clone())
            });

            Ok(aps)
        }
        Err(e) => {
            tracing::warn!("Wi-Fi scan failed: {e}");
            Ok(Vec::new())
        }
    }
}

/// Convert RSSI (dBm) to quality percentage [0-100].
fn rssi_to_quality(rssi: i32) -> u8 {
    match rssi {
        r if r >= -30 => 100,
        r if r <= -90 => 0,
        r => ((r + 90) as u8 * 100 / 60).min(100),
    }
}
