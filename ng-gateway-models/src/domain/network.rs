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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpMethod {
    Dhcp,
    Static,
    Disabled,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// STA + AP concurrency support.
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

// ─────────────────── Requests ───────────────────

/// Request to configure an interface's IP settings.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureInterfaceRequest {
    pub method: IpMethod,
    /// Required if method = static.
    pub ip_address: Option<IpAddr>,
    /// Required if method = static. CIDR prefix (e.g. 24 for /24).
    #[validate(range(min = 1, max = 32, message = "prefix must be in [1, 32]"))]
    pub prefix_length: Option<u8>,
    pub gateway: Option<IpAddr>,
    pub dns: Option<Vec<IpAddr>>,
}

/// Request to connect to a Wi-Fi network.
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
    /// 0 = auto channel selection. Valid 2.4 GHz channels: 1-13.
    pub channel: Option<u32>,
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
