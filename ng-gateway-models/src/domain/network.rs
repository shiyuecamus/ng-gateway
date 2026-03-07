use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use validator::Validate;

// ─────────────────── Enums ───────────────────

/// Network interface type determined by system API (D-Bus `DeviceType` on Linux).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceKind {
    Ethernet,
    Wifi,
    Loopback,
    Bridge,
    Vlan,
    Virtual,
    Unknown,
}

/// IP configuration method.
///
/// Used in read-only contexts (e.g. [`Ipv4Config`], [`DnsConfig`]) to describe
/// the current method. For write operations, prefer [`IpConfig`] which carries
/// the associated static configuration data as a tagged enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpMethod {
    Dhcp,
    Static,
    Disabled,
}

/// Static IP configuration fields.
///
/// Extracted as a standalone struct so `#[serde(rename_all = "camelCase")]` applies
/// correctly to all fields. Serde's internally-tagged enum `rename_all` only affects
/// variant name matching, **not** variant-interior field names — placing the fields
/// in a separate struct with its own `rename_all` attribute is the canonical fix.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticIpConfig {
    pub ip_address: IpAddr,
    pub prefix_length: u8,
    pub gateway: Option<IpAddr>,
    pub dns: Option<Vec<IpAddr>>,
}

/// Unified IP configuration using tagged enum for type safety.
///
/// DHCP variant carries no extra fields; Static variant enforces required fields
/// at compile time. Used by both wired interface configuration and Wi-Fi
/// connection requests, eliminating duplicated Option-field patterns.
///
/// # Serde Format
///
/// Internally tagged via `"method"`:
/// - `{ "method": "dhcp" }`
/// - `{ "method": "static", "ipAddress": "...", "prefixLength": 24, ... }`
/// - `{ "method": "disabled" }`
///
/// # Note on `rename_all`
///
/// The enum-level `rename_all` only renames variant discriminants in the tag.
/// The `Static` variant's fields live in [`StaticIpConfig`] which carries its
/// own `rename_all = "camelCase"`, ensuring correct JSON field names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum IpConfig {
    /// Obtain IP configuration automatically via DHCP.
    #[serde(rename = "dhcp")]
    Dhcp,
    /// Manual static IP configuration.
    #[serde(rename = "static")]
    Static {
        /// Flattened static IP fields — keeps the JSON shape flat
        /// (`{ "method": "static", "ipAddress": "...", ... }`).
        #[serde(flatten)]
        config: StaticIpConfig,
    },
    /// IP stack disabled on this interface.
    #[serde(rename = "disabled")]
    Disabled,
}

impl Validate for IpConfig {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        if let Self::Static { config } = self {
            if config.prefix_length < 1 || config.prefix_length > 32 {
                let mut errors = validator::ValidationErrors::new();
                let mut err = validator::ValidationError::new("range");
                err.message = Some("prefix must be in [1, 32]".into());
                errors.add("prefix_length", err);
                return Err(errors);
            }
        }
        Ok(())
    }
}

impl IpConfig {
    /// Return the corresponding [`IpMethod`] discriminant.
    #[inline]
    pub fn method(&self) -> IpMethod {
        match self {
            Self::Dhcp => IpMethod::Dhcp,
            Self::Static { .. } => IpMethod::Static,
            Self::Disabled => IpMethod::Disabled,
        }
    }
}

/// Interface operational state derived from kernel link flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    Up,
    Down,
    Dormant,
    Unknown,
}

/// Wi-Fi security type derived from NM AP flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WifiSecurity {
    Open,
    Wep,
    WpaPsk,
    Wpa2Psk,
    Wpa3Sae,
    WpaEnterprise,
    Wpa2Enterprise,
    Unknown,
}

/// Wi-Fi frequency band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiBand {
    #[serde(rename = "2.4ghz")]
    Band2_4Ghz,
    #[serde(rename = "5ghz")]
    Band5Ghz,
    #[serde(rename = "6ghz")]
    Band6Ghz,
    Unknown,
}

/// Wi-Fi operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiMode {
    /// Station (client) mode.
    Station,
    /// Access Point mode.
    Ap,
    /// Ad-hoc mode.
    AdHoc,
    Unknown,
}

/// STA + AP concurrency support level detected at runtime via `iw phy info`.
///
/// This is the raw hardware capability value used for diagnostics.
/// For UI decision-making, prefer [`ApMode`] which provides clearer semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaApCapability {
    /// Single card supports simultaneous STA + AP.
    SingleCardConcurrent,
    /// Requires two separate wireless interfaces.
    DualCard,
    /// STA + AP not supported.
    NotSupported,
    /// Unable to determine (macOS/Windows or detection failed).
    Unknown,
}

/// High-level AP operating mode derived from hardware capabilities.
///
/// This drives UI behavior (warnings, confirmations) and backend logic
/// (whether to disconnect STA before starting AP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApMode {
    /// STA + AP concurrent on a single card — no STA disruption.
    Concurrent,
    /// AP uses a dedicated second wireless card — no STA disruption.
    DedicatedCard,
    /// AP and STA are mutually exclusive (single card, driver doesn't support concurrency).
    /// Starting AP will disconnect the current Wi-Fi STA connection.
    Exclusive,
    /// AP is not available (no wireless hardware or no AP mode support).
    Unavailable,
}

/// Action to perform on the AP hotspot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApAction {
    Start,
    Stop,
}

/// Request to start or stop the AP hotspot.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlApRequest {
    pub action: ApAction,
}

/// Platform network management capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformSupport {
    /// Full support (Linux with NetworkManager).
    Full,
    /// Read-only (macOS/Windows dev environment).
    ReadOnly,
    /// Not available.
    Unavailable,
}

// ─────────────────── Responses ───────────────────

/// IPv4 address with CIDR prefix length.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ipv4AddressInfo {
    pub address: IpAddr,
    pub prefix_length: u8,
}

/// IPv4 configuration for an interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ipv4Config {
    pub addresses: Vec<Ipv4AddressInfo>,
    pub gateway: Option<IpAddr>,
    pub dns: Vec<IpAddr>,
    pub method: IpMethod,
}

/// IPv6 address with CIDR prefix length.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ipv6AddressInfo {
    pub address: IpAddr,
    pub prefix_length: u8,
}

/// IPv6 configuration for an interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ipv6Config {
    pub addresses: Vec<Ipv6AddressInfo>,
    pub gateway: Option<IpAddr>,
    pub dns: Vec<IpAddr>,
    pub method: IpMethod,
}

/// Network interface summary (list view).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInterfaceSummary {
    /// System interface name (e.g. "eth0", "wlan0", "enp2s0").
    pub name: String,
    /// Human-readable display name.
    pub display_name: Option<String>,
    pub kind: InterfaceKind,
    pub link_state: LinkState,
    pub mac_address: Option<String>,
    pub ipv4: Option<Ipv4Config>,
    pub ipv6: Option<Ipv6Config>,
    /// Wi-Fi mode (if wireless).
    pub wifi_mode: Option<WifiMode>,
    /// Connected SSID (if Wi-Fi STA mode).
    pub connected_ssid: Option<String>,
    /// AP SSID (if Wi-Fi AP mode).
    pub ap_ssid: Option<String>,
    /// Signal strength in dBm (if Wi-Fi STA).
    pub signal_dbm: Option<i32>,
    /// Signal quality percentage [0-100].
    pub signal_quality: Option<u8>,
    /// Link speed in Mbps.
    pub speed_mbps: Option<u32>,
    /// Rx bytes since boot.
    pub rx_bytes: Option<u64>,
    /// Tx bytes since boot.
    pub tx_bytes: Option<u64>,
}

/// Detailed network interface info (detail view).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInterfaceDetail {
    #[serde(flatten)]
    pub summary: NetworkInterfaceSummary,
    /// NetworkManager connection UUID (Linux only).
    pub nm_connection_uuid: Option<String>,
    /// MTU.
    pub mtu: Option<u32>,
    /// Driver name.
    pub driver: Option<String>,
    /// Firmware version.
    pub firmware_version: Option<String>,
    /// Rx/Tx packet counters.
    pub rx_packets: Option<u64>,
    pub tx_packets: Option<u64>,
    pub rx_errors: Option<u64>,
    pub tx_errors: Option<u64>,
}

/// Wi-Fi scan result entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiAccessPoint {
    /// SSID (may be empty for hidden networks).
    pub ssid: String,
    /// BSSID (MAC of AP).
    pub bssid: String,
    pub security: WifiSecurity,
    pub band: WifiBand,
    /// Channel number.
    pub channel: u32,
    /// Frequency in MHz.
    pub frequency: u32,
    /// Signal strength in dBm.
    pub signal_dbm: i32,
    /// Signal quality percentage [0-100].
    pub signal_quality: u8,
    /// Maximum bit rate in Kbps.
    pub max_bitrate_kbps: Option<u32>,
    /// Whether this AP is currently connected.
    pub is_connected: bool,
}

/// Wi-Fi STA connection status.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiStaStatus {
    pub connected: bool,
    pub interface_name: Option<String>,
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub security: Option<WifiSecurity>,
    pub band: Option<WifiBand>,
    pub channel: Option<u32>,
    pub frequency: Option<u32>,
    pub signal_dbm: Option<i32>,
    pub signal_quality: Option<u8>,
    pub ip_address: Option<IpAddr>,
    pub gateway: Option<IpAddr>,
    pub dns: Vec<IpAddr>,
    pub speed_mbps: Option<u32>,
    /// Connected duration in seconds.
    pub connected_secs: Option<u64>,
}

/// Saved Wi-Fi connection profile from NetworkManager.
///
/// Represents a persistent NM connection of type `802-11-wireless`.
/// Returned by `list_saved_wifi_connections` for the "saved networks" UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedWifiConnection {
    /// NetworkManager connection UUID (stable identifier).
    pub uuid: String,
    /// SSID of the saved network.
    pub ssid: String,
    /// Whether this connection is currently active.
    pub is_active: bool,
    /// Whether NetworkManager will auto-connect to this network.
    pub autoconnect: bool,
    /// Security type configured for this connection.
    pub security: WifiSecurity,
    /// IPv4 configuration (DHCP/Static/Disabled) saved in the profile.
    pub ip_config: IpConfig,
    /// Unix timestamp (seconds) of the last successful connection, if known.
    pub last_connected: Option<u64>,
}

/// Pre-flight check result for Wi-Fi connect operations.
///
/// Returned by the preflight endpoint so the frontend can display appropriate
/// confirmation dialogs before proceeding with `connect_wifi`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiConnectPreflight {
    /// Target SSID the user wants to connect to.
    pub ssid: String,
    /// Whether the AP hotspot is currently active and will be stopped to free the interface.
    pub ap_will_stop: bool,
    /// Whether the current management connection (the one serving this Web UI) will be lost.
    /// True when the user is connected via the AP hotspot and the AP will be stopped.
    pub connection_will_be_lost: bool,
    /// Whether the AP can be automatically restored on a virtual interface after
    /// the STA connection succeeds (probe detected concurrent STA+AP support).
    pub ap_can_restore: bool,
    /// Human-readable warnings for the frontend to display.
    pub warnings: Vec<String>,
}

/// AP hotspot status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApStatus {
    pub active: bool,
    pub interface_name: Option<String>,
    pub ssid: Option<String>,
    pub band: Option<WifiBand>,
    pub channel: Option<u32>,
    pub frequency: Option<u32>,
    pub security: Option<WifiSecurity>,
    /// Number of connected clients.
    pub connected_clients: Option<u32>,
    /// IP address of the AP interface.
    pub ip_address: Option<IpAddr>,
    /// Subnet mask prefix length.
    pub prefix_length: Option<u8>,
    /// High-level AP operating mode.
    pub ap_mode: ApMode,
    /// Whether starting the AP will disconnect the current Wi-Fi STA connection.
    pub sta_will_disconnect: bool,
    /// Set to `true` if a previous `stop_ap` failed to restore the STA connection.
    /// The frontend should prompt the user to restart the network or reboot.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sta_restore_failed: bool,
}

/// DNS configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsConfig {
    /// Global DNS servers.
    pub servers: Vec<IpAddr>,
    /// Search domains.
    pub search_domains: Vec<String>,
    /// DNS mode (auto from DHCP or manual).
    pub mode: IpMethod,
}

/// Platform capabilities response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkCapabilities {
    /// Platform support level.
    pub platform: PlatformSupport,
    /// Operating system type.
    pub os: String,
    /// Architecture.
    pub arch: String,
    /// Whether NetworkManager is available.
    pub network_manager_available: bool,
    /// NetworkManager version (if available).
    pub network_manager_version: Option<String>,
    /// Whether interface configuration is supported.
    pub can_configure_interfaces: bool,
    /// Whether Wi-Fi scanning is supported.
    pub can_scan_wifi: bool,
    /// Whether Wi-Fi STA connection is supported.
    pub can_connect_wifi: bool,
    /// Whether AP management is supported.
    pub can_manage_ap: bool,
    /// High-level AP operating mode for UI decision-making.
    pub ap_mode: ApMode,
    /// Raw STA + AP concurrency support (diagnostics).
    pub sta_ap_capability: StaApCapability,
    /// List of wireless interfaces with their capabilities.
    pub wireless_interfaces: Vec<WirelessInterfaceCapability>,
}

/// Wireless interface capability info.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WirelessInterfaceCapability {
    pub name: String,
    pub phy: String,
    /// Supported modes: "managed", "AP", "monitor", etc.
    pub supported_modes: Vec<String>,
    /// Whether this interface supports simultaneous STA + AP.
    pub supports_sta_ap_concurrent: bool,
    /// Supported bands.
    pub supported_bands: Vec<WifiBand>,
    /// Current mode.
    pub current_mode: Option<WifiMode>,
}

// ─────────────────── Path / Query Params ───────────────────

/// Path parameter for interface name in REST routes (e.g. `/interfaces/{name}`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceNamePath {
    /// System interface name (e.g. "eth0", "wlan0", "enp2s0").
    pub name: String,
}

/// Optional query parameter for Wi-Fi interface selection.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiInterfaceQuery {
    /// Specific wireless interface to use (defaults to first available).
    pub interface: Option<String>,
}

// ─────────────────── Requests ───────────────────

/// Request to configure an interface's IP settings.
///
/// Uses the unified [`IpConfig`] tagged enum to enforce type-safe IP configuration:
/// the `Static` variant requires IP address and prefix length at the type level.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureInterfaceRequest {
    /// IP configuration (DHCP, Static, or Disabled).
    #[validate(nested)]
    pub ip_config: IpConfig,
}

/// Request to connect to a Wi-Fi network.
///
/// Supports optional static IP configuration via [`IpConfig`]. When `ip_config`
/// is `None` or omitted, DHCP is used (backward-compatible default).
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct WifiConnectRequest {
    #[validate(length(min = 1, max = 32, message = "SSID length must be in [1, 32]"))]
    pub ssid: String,
    /// Password (WPA-PSK). Required for non-open networks.
    pub password: Option<String>,
    /// Specific BSSID to connect to (optional, for multi-AP same-SSID scenarios).
    pub bssid: Option<String>,
    /// Force hidden network (send probe with SSID).
    pub hidden: Option<bool>,
    /// Which wireless interface to use (defaults to first available STA interface).
    pub interface_name: Option<String>,
    /// IP configuration for this Wi-Fi connection. Defaults to DHCP when absent.
    #[validate(nested)]
    pub ip_config: Option<IpConfig>,
}

/// Request to disconnect from the current Wi-Fi STA connection.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiDisconnectRequest {
    /// Specific wireless interface to disconnect (defaults to first available).
    pub interface_name: Option<String>,
    /// When true, sets `autoconnect=false` on the NM connection profile to prevent
    /// NetworkManager from automatically reconnecting. The flag is restored when the
    /// user manually reconnects via `connect_wifi`.
    #[serde(default)]
    pub disable_autoconnect: bool,
}

/// Request to forget (delete) a saved Wi-Fi connection profile.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgetWifiRequest {
    /// NetworkManager connection UUID to delete.
    pub uuid: String,
}

/// Path parameter for Wi-Fi connection UUID in REST routes.
#[derive(Debug, Clone, Deserialize)]
pub struct WifiUuidPath {
    /// NetworkManager connection UUID.
    pub uuid: String,
}

/// Request to modify AP hotspot configuration.
///
/// Band is fixed to 2.4 GHz and not configurable via API.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureApRequest {
    #[validate(length(min = 1, max = 32, message = "SSID length must be in [1, 32]"))]
    pub ssid: Option<String>,
    /// WPA2 password (8-63 chars).
    #[validate(length(min = 8, max = 63, message = "password length must be in [8, 63]"))]
    pub password: Option<String>,
    /// Wi-Fi channel number. 0 = auto (picks a sensible default based on hardware).
    /// 2.4 GHz: 1-13, 5 GHz: 36-165 (availability depends on regulatory domain).
    pub channel: Option<u32>,
    /// ISO 3166-1 alpha-2 Wi-Fi regulatory country code (e.g. "CN", "US", "JP").
    ///
    /// Overrides the value from `gateway.toml` (`general.wifi_country_code`) for this
    /// configuration update. When omitted, the configured or default value is used.
    #[validate(length(
        min = 2,
        max = 2,
        message = "country code must be exactly 2 characters"
    ))]
    pub country_code: Option<String>,
    /// Whether to restart the AP after configuration change.
    pub restart: Option<bool>,
}

/// Request to modify DNS configuration.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureDnsRequest {
    pub servers: Vec<IpAddr>,
    pub search_domains: Option<Vec<String>>,
}

// ─────────────────── Aggregated Status (best-interface) ───────────────────

/// Pre-selected best wired interface with full status.
///
/// The backend selects the "best" ethernet interface (prefer connected/up, then first),
/// so the frontend does not need to do interface selection logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WiredStatus {
    /// Whether any wired interface was found.
    pub available: bool,
    /// The selected best interface (if available).
    pub interface: Option<NetworkInterfaceSummary>,
    /// All ethernet interfaces (for advanced users / interface picker).
    pub all_interfaces: Vec<NetworkInterfaceSummary>,
}
