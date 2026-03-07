//! NetworkManager D-Bus constants.
//!
//! Centralizes all D-Bus interface names, property keys, connection setting keys,
//! and NM enum constants used by [`super::linux::LinuxNetworkManager`].
//!
//! # Organization
//! - [`iface`] — D-Bus interface names passed to `Properties.Get` / `Properties.GetAll`.
//! - [`prop`] — Property keys within those interfaces.
//! - [`conn`] — Connection settings keys used with `GetSettings` / `AddAndActivateConnection`.
//! - [`method`] — NM `ipv4.method` / `ipv6.method` string values.
//! - [`device_type`] — `NM_DEVICE_TYPE_*` integer constants.
//! - [`device_state`] — `NM_DEVICE_STATE_*` integer constants.
//! - [`active_conn_state`] — `NM_ACTIVE_CONNECTION_STATE_*` integer constants.
//! - [`ap_sec`] — `NM_802_11_AP_SEC_*` flag constants for Wi-Fi security detection.
//! - [`unit_mode`] — systemd D-Bus StartUnit/StopUnit mode strings.

/// D-Bus interface names for NetworkManager objects.
pub mod iface {
    /// Root NetworkManager interface.
    pub const NM: &str = "org.freedesktop.NetworkManager";
    /// Generic device properties.
    pub const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
    /// Wireless-specific device properties.
    pub const DEVICE_WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
    /// Active connection properties.
    pub const ACTIVE_CONN: &str = "org.freedesktop.NetworkManager.Connection.Active";
    /// Settings root — manages all saved connection profiles.
    pub const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
    /// Settings (saved) connection profile.
    pub const SETTINGS_CONN: &str = "org.freedesktop.NetworkManager.Settings.Connection";
    /// IPv4 configuration object.
    pub const IP4_CONFIG: &str = "org.freedesktop.NetworkManager.IP4Config";
    /// IPv6 configuration object.
    pub const IP6_CONFIG: &str = "org.freedesktop.NetworkManager.IP6Config";
    /// Scanned access point object.
    pub const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
}

/// NM D-Bus property keys.
///
/// These are the string keys passed to `Properties.Get(interface, key)` or
/// looked up from the `HashMap` returned by `Properties.GetAll`.
pub mod prop {
    // ─── Device ───
    pub const INTERFACE: &str = "Interface";
    pub const DEVICE_TYPE: &str = "DeviceType";
    pub const STATE: &str = "State";
    pub const HW_ADDRESS: &str = "HwAddress";
    pub const SPEED: &str = "Speed";
    pub const MTU: &str = "Mtu";
    pub const DRIVER: &str = "Driver";
    pub const ACTIVE_CONNECTION: &str = "ActiveConnection";
    pub const IP4_CONFIG: &str = "Ip4Config";
    pub const IP6_CONFIG: &str = "Ip6Config";

    // ─── Device.Wireless ───
    pub const MODE: &str = "Mode";
    pub const ACTIVE_ACCESS_POINT: &str = "ActiveAccessPoint";
    pub const LAST_SCAN: &str = "LastScan";

    // ─── AccessPoint ───
    pub const SSID: &str = "Ssid";
    pub const STRENGTH: &str = "Strength";
    pub const FREQUENCY: &str = "Frequency";
    pub const MAX_BITRATE: &str = "MaxBitrate";
    pub const FLAGS: &str = "Flags";
    pub const WPA_FLAGS: &str = "WpaFlags";
    pub const RSN_FLAGS: &str = "RsnFlags";

    // ─── ActiveConnection ───
    pub const CONNECTION: &str = "Connection";
    pub const UUID: &str = "Uuid";

    // ─── IP Config ───
    pub const GATEWAY: &str = "Gateway";
    pub const ADDRESS_DATA: &str = "AddressData";
    pub const NAMESERVER_DATA: &str = "NameserverData";

    // ─── Dict keys inside AddressData / NameserverData entries ───
    pub const ADDR_KEY_ADDRESS: &str = "address";
    pub const ADDR_KEY_PREFIX: &str = "prefix";
}

/// NM connection settings keys.
///
/// Used when building the `HashMap<&str, HashMap<&str, Value>>` for
/// `AddAndActivateConnection` and when parsing `GetSettings` results.
pub mod conn {
    // ─── Top-level setting groups ───
    pub const CONNECTION: &str = "connection";
    pub const WIFI: &str = "802-11-wireless";
    pub const WIFI_SECURITY: &str = "802-11-wireless-security";
    pub const ETHERNET: &str = "802-3-ethernet";
    pub const IPV4: &str = "ipv4";
    pub const IPV6: &str = "ipv6";

    // ─── "connection" group keys ───
    pub const ID: &str = "id";
    pub const TYPE: &str = "type";
    pub const INTERFACE_NAME: &str = "interface-name";
    pub const AUTOCONNECT: &str = "autoconnect";

    // ─── "802-11-wireless" group keys ───
    pub const WIFI_SSID: &str = "ssid";
    pub const WIFI_MODE: &str = "mode";
    pub const WIFI_HIDDEN: &str = "hidden";
    pub const WIFI_BSSID: &str = "bssid";

    // ─── "802-11-wireless-security" group keys ───
    pub const KEY_MGMT: &str = "key-mgmt";
    pub const PSK: &str = "psk";

    // ─── IP group keys ───
    pub const METHOD: &str = "method";
    pub const ADDRESS_DATA: &str = "address-data";
    pub const DNS_DATA: &str = "dns-data";

    // ─── "connection" group keys (continued) ───
    pub const TIMESTAMP: &str = "timestamp";
}

/// NM IPv4/IPv6 method string values.
pub mod method {
    pub const AUTO: &str = "auto";
    pub const MANUAL: &str = "manual";
    pub const DISABLED: &str = "disabled";
}

/// NM Wi-Fi connection mode string values.
pub mod wifi_mode {
    pub const INFRASTRUCTURE: &str = "infrastructure";
}

/// NM Wi-Fi security key management string values.
pub mod key_mgmt {
    pub const WPA_PSK: &str = "wpa-psk";
}

/// NM D-Bus method names used via `call_method`.
pub mod dbus_method {
    pub const GET_SETTINGS: &str = "GetSettings";
    pub const DELETE: &str = "Delete";
    pub const UPDATE: &str = "Update";
    pub const REQUEST_SCAN: &str = "RequestScan";
    pub const GET_ALL_ACCESS_POINTS: &str = "GetAllAccessPoints";
    pub const LIST_CONNECTIONS: &str = "ListConnections";
}

/// NM Settings D-Bus path.
pub mod settings_path {
    pub const ROOT: &str = "/org/freedesktop/NetworkManager/Settings";
}

/// `NM_DEVICE_TYPE_*` constants.
pub mod device_type {
    pub const ETHERNET: u32 = 1;
    pub const WIFI: u32 = 2;
    pub const VLAN: u32 = 11;
    pub const BRIDGE: u32 = 13;
}

/// `NM_DEVICE_STATE_*` constants.
pub mod device_state {
    pub const UNMANAGED: u32 = 10;
    pub const UNAVAILABLE: u32 = 20;
    pub const DISCONNECTED: u32 = 30;
    pub const ACTIVATED: u32 = 100;
}

/// `NM_ACTIVE_CONNECTION_STATE_*` constants.
pub mod active_conn_state {
    pub const ACTIVATED: u32 = 2;
    pub const DEACTIVATED: u32 = 4;
}

/// `NM_802_11_AP_SEC_*` flag constants for Wi-Fi security classification.
pub mod ap_sec {
    pub const KEY_MGMT_PSK: u32 = 0x100;
    pub const KEY_MGMT_802_1X: u32 = 0x200;
    pub const KEY_MGMT_SAE: u32 = 0x400;
}

/// systemd D-Bus unit management mode strings.
pub mod unit_mode {
    pub const REPLACE: &str = "replace";
}

/// systemd D-Bus interface and property constants.
pub mod systemd {
    pub const UNIT_IFACE: &str = "org.freedesktop.systemd1.Unit";
    pub const ACTIVE_STATE: &str = "ActiveState";

    /// Unit active states.
    pub const STATE_ACTIVE: &str = "active";
    pub const STATE_INACTIVE: &str = "inactive";
    pub const STATE_FAILED: &str = "failed";
}
