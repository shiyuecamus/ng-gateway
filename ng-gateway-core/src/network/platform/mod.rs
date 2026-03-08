//! Platform-abstracted network management.
//!
//! Each target OS has its own implementation of [`PlatformNetworkManager`].
//! The correct implementation is selected at compile time via `cfg` attributes,
//! with runtime capability detection for feature availability.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
pub mod nm_dbus;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::LinuxNetworkManager;
#[cfg(target_os = "macos")]
pub use macos::MacosNetworkManager;
#[cfg(target_os = "windows")]
pub use windows::WindowsNetworkManager;

use async_trait::async_trait;
#[cfg(target_os = "linux")]
use ng_gateway_error::NGError;
use ng_gateway_error::NGResult;
use ng_gateway_models::domain::prelude::{
    ApStatus, ConfigureApRequest, ConfigureInterfaceRequest, ForgetWifiRequest,
    NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary, SavedWifiConnection,
    WifiAccessPoint, WifiConnectPreflight, WifiConnectRequest, WifiDisconnectRequest,
    WifiStaStatus,
};
#[cfg(target_os = "linux")]
use ng_gateway_models::domain::prelude::{WifiBand, WifiSecurity};

/// Platform-abstracted network management interface.
///
/// # Design
/// - **Linux** (`Full`): implemented via NetworkManager D-Bus API (`zbus`).
///   Supports all operations including AP hotspot management.
/// - **macOS** (`Partial`): implemented via CoreWLAN + SystemConfiguration native APIs.
///   CLI (`networksetup`) retained only for IP configuration writes.
///   Supports interface configuration, Wi-Fi STA connect/disconnect, saved profiles.
///   AP management is not supported (macOS has no accessible AP mode).
/// - **Windows** (`Partial`): implemented via Native Wifi API + GetAdaptersAddresses.
///   CLI (`netsh`) retained only for IP configuration writes.
///   Supports interface configuration, Wi-Fi STA connect/disconnect, saved profiles.
///   AP management is not supported (`hostednetwork` is deprecated since Win10).
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

    /// Pre-flight check for a Wi-Fi connect operation.
    ///
    /// Returns information about side-effects (AP stop, connection loss) so the
    /// frontend can display an appropriate confirmation dialog before proceeding.
    async fn wifi_connect_preflight(
        &self,
        request: &WifiConnectRequest,
    ) -> NGResult<WifiConnectPreflight>;

    /// Connect to a Wi-Fi network as STA.
    ///
    /// When AP is running in exclusive mode, this will orchestrate: stop AP →
    /// switch to managed mode → connect STA. If the driver actually supports
    /// concurrent STA+AP (detected via probe), AP will be restored on a
    /// virtual interface after STA is connected.
    async fn connect_wifi(&self, request: &WifiConnectRequest) -> NGResult<WifiStaStatus>;

    /// Disconnect Wi-Fi STA on the given (or default) interface.
    ///
    /// When `disable_autoconnect` is set, updates the NM connection profile to
    /// prevent automatic reconnection. After disconnection, evaluates whether the
    /// AP hotspot should be restored as a fallback management channel.
    async fn disconnect_wifi(&self, request: &WifiDisconnectRequest) -> NGResult<()>;

    /// Get current Wi-Fi STA connection status.
    async fn wifi_sta_status(&self, interface_name: Option<&str>) -> NGResult<WifiStaStatus>;

    /// List all saved Wi-Fi connection profiles known to NetworkManager.
    ///
    /// Returns connection metadata including UUID, SSID, autoconnect flag,
    /// security type, IP configuration, and last-connected timestamp.
    async fn list_saved_wifi_connections(&self) -> NGResult<Vec<SavedWifiConnection>>;

    /// Delete (forget) a saved Wi-Fi connection profile by UUID.
    ///
    /// If the connection is currently active, deactivates it first, then deletes
    /// the profile from NetworkManager. After deletion, evaluates AP restore.
    async fn forget_wifi(&self, request: &ForgetWifiRequest) -> NGResult<()>;

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

/// Cross-platform Wi-Fi scan fallback using the `wifi_scan` crate.
///
/// On macOS and Windows, the platform managers now use their own native scan
/// implementations (CoreWLAN / Native Wifi API). This function is retained as
/// a fallback for Linux (nl80211) when the NM D-Bus scan path is unavailable.
///
/// Runs a blocking scan in a `spawn_blocking` task to avoid blocking the async runtime.
#[cfg(target_os = "linux")]
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
                    let channel = w.channel;
                    let bssid = w.mac;
                    let ssid = w.ssid;
                    let redacted_identifiers = ssid.is_empty() && bssid.is_empty();
                    let ssid = if redacted_identifiers {
                        format!("<redacted> ch{channel}")
                    } else {
                        ssid
                    };

                    WifiAccessPoint {
                        ssid,
                        bssid,
                        security,
                        band,
                        channel,
                        frequency,
                        signal_dbm: w.signal_level,
                        signal_quality,
                        max_bitrate_kbps: None,
                        is_connected: false,
                    }
                })
                .collect();

            // Remove truly empty SSIDs, sort by signal descending, then deduplicate.
            aps.retain(|ap| !ap.ssid.is_empty());
            aps.sort_unstable_by(|a, b| {
                a.ssid
                    .cmp(&b.ssid)
                    .then(a.bssid.cmp(&b.bssid))
                    .then(a.channel.cmp(&b.channel))
                    .then(b.signal_quality.cmp(&a.signal_quality))
            });
            aps.dedup_by(|a, b| a.ssid == b.ssid && a.bssid == b.bssid && a.channel == b.channel);
            aps.sort_unstable_by(|a, b| b.signal_quality.cmp(&a.signal_quality));

            Ok(aps)
        }
        Err(e) => {
            tracing::warn!("Wi-Fi scan failed: {e}");
            Err(NGError::Error(format!("Wi-Fi scan failed: {e}")))
        }
    }
}

/// Convert RSSI (dBm) to quality percentage [0-100].
///
/// Handles cross-platform differences: some drivers/platforms report signal
/// as a percentage (0-100) rather than dBm. Values in [0, 100] are treated
/// as already-converted quality; negative values are treated as dBm.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn rssi_to_quality(rssi: i32) -> u8 {
    if rssi >= 0 {
        // Already a percentage (e.g., macOS CoreWLAN or some Windows drivers).
        (rssi as u8).min(100)
    } else {
        match rssi {
            r if r >= -30 => 100,
            r if r <= -90 => 0,
            r => ((r + 90) as u8 * 100 / 60).min(100),
        }
    }
}
