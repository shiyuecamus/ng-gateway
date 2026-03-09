//! Linux network manager using NetworkManager D-Bus API (`zbus`).
//!
//! This is the primary production implementation providing full network management:
//! - Interface enumeration via `org.freedesktop.NetworkManager.GetDevices()`
//! - Wi-Fi scanning via `Device.Wireless.RequestScan()` + `GetAllAccessPoints()`
//! - Wi-Fi connection via `AddAndActivateConnection()`
//! - Interface configuration via connection profile updates
//! - AP hotspot management (Phase 4)
//!
//! All D-Bus operations are asynchronous via `zbus` + tokio runtime.

use crate::network::{
    ap_config::{self, ApRenderContext, AP_CONFIG_DIR, AP_ENV_FILE, HOSTAPD_CONF_FILE},
    ap_manager::{ApServiceManager, ApServiceStatus},
    capability::{
        aggregate_sta_ap_capability, detect_phy_capabilities, determine_ap_mode, resolve_phy_name,
    },
    platform::{
        nm_dbus::{
            active_conn_state, ap_sec, conn, dbus_method, device_state, device_type, iface,
            key_mgmt, method, prop, settings_path, wifi_mode,
        },
        PlatformNetworkManager,
    },
};
use async_trait::async_trait;
use ng_gateway_error::{network::NetworkError, NGResult};
use ng_gateway_models::domain::prelude::{
    ApMode, ApStatus, ConfigureApRequest, ConfigureInterfaceRequest, ForgetWifiRequest,
    InterfaceKind, IpConfig, IpMethod, Ipv4AddressInfo, Ipv4Config, Ipv6AddressInfo, Ipv6Config,
    LinkState, NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary,
    PlatformSupport, SavedWifiConnection, StaticIpConfig, WifiAccessPoint, WifiBand,
    WifiConnectPreflight, WifiConnectRequest, WifiDisconnectRequest, WifiMode, WifiSecurity,
    WifiStaStatus, WirelessInterfaceCapability,
};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    net::{IpAddr, Ipv4Addr},
    path::Path,
    process, str,
    time::{Duration, Instant},
};
use tokio::{sync::RwLock, time::sleep};
use tracing::{debug, info, warn};
use zbus::{
    proxy,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
    Connection,
};

// ─── D-Bus Proxy Definitions ───
//
// Note: `#[proxy]` macro attributes require string literals — we cannot
// use `iface::*` constants inside the macro invocation.

/// Proxy for `org.freedesktop.NetworkManager` root interface.
#[proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(name = "ActivateConnection")]
    fn activate_connection(
        &self,
        connection: &ObjectPath<'_>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<OwnedObjectPath>;

    #[zbus(name = "AddAndActivateConnection")]
    fn add_and_activate_connection(
        &self,
        connection: HashMap<&str, HashMap<&str, Value<'_>>>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;

    #[zbus(name = "DeactivateConnection")]
    fn deactivate_connection(&self, active_connection: &ObjectPath<'_>) -> zbus::Result<()>;

    #[zbus(name = "GetDeviceByIpIface")]
    fn get_device_by_ip_iface(&self, iface: &str) -> zbus::Result<OwnedObjectPath>;

    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;
}

/// Proxy for generic D-Bus properties access.
#[proxy(
    interface = "org.freedesktop.DBus.Properties",
    default_service = "org.freedesktop.NetworkManager"
)]
trait NMProperties {
    fn get(&self, interface_name: &str, property_name: &str) -> zbus::Result<OwnedValue>;

    fn get_all(&self, interface_name: &str) -> zbus::Result<HashMap<String, OwnedValue>>;
}

// ─── Linux-specific path and protocol constants ───

/// sysfs paths and statistic counter names.
mod sysfs {
    pub const NET_DIR: &str = "/sys/class/net";
    pub const STATISTICS_DIR: &str = "statistics";
    pub const RX_BYTES: &str = "rx_bytes";
    pub const TX_BYTES: &str = "tx_bytes";
    pub const RX_PACKETS: &str = "rx_packets";
    pub const TX_PACKETS: &str = "tx_packets";
    pub const RX_ERRORS: &str = "rx_errors";
    pub const TX_ERRORS: &str = "tx_errors";

    /// Build the full sysfs stat path for a given interface and counter.
    pub fn stat_path(iface: &str, stat: &str) -> String {
        format!("{NET_DIR}/{iface}/{STATISTICS_DIR}/{stat}")
    }
}

/// hostapd control interface constants.
mod hostapd_ctrl {
    pub const CTRL_DIR: &str = "/var/run/hostapd";
    pub const CLIENT_PATH_PREFIX: &str = "/tmp/ng-gw-hapd";
    pub const CMD_STA_FIRST: &str = "STA-FIRST";
    pub const CMD_STA_NEXT: &str = "STA-NEXT";
    pub const RESP_FAIL: &str = "FAIL";
}

/// Runtime state directory for persisting transient data across restarts.
const RUNTIME_STATE_DIR: &str = "/run/ng-gateway";

/// File used to persist the stashed STA connection across gateway restarts.
const STA_RESTORE_FILE: &str = "sta-restore.json";

/// Stashed STA connection info for restore-after-stop-AP in exclusive mode.
///
/// When starting AP in exclusive mode we disconnect STA; we save the NM connection
/// and device paths so we can reactivate via `ActivateConnection` when the user stops AP.
/// This is also persisted to disk so that a gateway restart does not lose the restore info.
///
/// In addition to the bare routing info, we capture a lightweight IPv4 fingerprint
/// from the NM profile at stash time. This is **not** a second source of truth —
/// the NM profile remains canonical — but it lets the restore path verify that the
/// connection was re-established with the expected IP strategy (static vs DHCP) and,
/// if not, trigger a corrective re-activation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StashedStaConnection {
    /// Settings connection path (org.freedesktop.NetworkManager.Settings.Connection).
    connection_path: String,
    /// Wi-Fi device path (org.freedesktop.NetworkManager.Device).
    device_path: String,
    /// Stable NM profile UUID — survives NM restarts and object-path renumbering.
    /// Used as a fallback locator when `connection_path` becomes stale.
    #[serde(default)]
    connection_uuid: Option<String>,
    /// Lightweight IPv4 fingerprint captured at stash time.
    /// Used for post-restore verification only — never written back to NM.
    #[serde(default)]
    ipv4_fingerprint: Option<Ipv4Fingerprint>,
}

/// Lightweight snapshot of the IPv4 configuration from an NM connection profile.
///
/// Captured at stash time and compared after restore to detect mismatches
/// (e.g. NM autoconnect selected a different profile, or the profile was
/// unexpectedly reverted to DHCP).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Ipv4Fingerprint {
    /// `"manual"` (static) or `"auto"` (DHCP).
    method: String,
    /// First static IP address (if method == manual).
    address: Option<String>,
    /// Prefix length (if method == manual).
    prefix: Option<u8>,
    /// Gateway (if method == manual).
    gateway: Option<String>,
}

/// Saved autoconnect state for a profile temporarily modified during STA restore.
///
/// We use this to suppress competing Wi-Fi profiles while explicitly restoring
/// the stashed profile, then restore the original autoconnect flags afterward.
#[derive(Debug, Clone)]
struct AutoconnectSuppression {
    settings_path: String,
    previous_autoconnect: bool,
}

impl StashedStaConnection {
    /// Persist to `/run/ng-gateway/sta-restore.json`.
    async fn persist(&self) {
        if let Err(e) = tokio::fs::create_dir_all(RUNTIME_STATE_DIR).await {
            warn!("Failed to create runtime state dir: {e}");
            return;
        }
        let path = format!("{RUNTIME_STATE_DIR}/{STA_RESTORE_FILE}");
        match serde_json::to_string(self) {
            Ok(json) => {
                if let Err(e) = tokio::fs::write(&path, json).await {
                    warn!("Failed to persist STA restore info: {e}");
                }
            }
            Err(e) => warn!("Failed to serialize STA restore info: {e}"),
        }
    }

    /// Load from disk (best-effort).
    async fn load() -> Option<Self> {
        let path = format!("{RUNTIME_STATE_DIR}/{STA_RESTORE_FILE}");
        let content = tokio::fs::read_to_string(&path).await.ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Remove the persisted file.
    async fn remove() {
        let path = format!("{RUNTIME_STATE_DIR}/{STA_RESTORE_FILE}");
        let _ = tokio::fs::remove_file(&path).await;
    }
}

/// Linux network manager backed by NetworkManager D-Bus.
pub struct LinuxNetworkManager {
    dbus_conn: Connection,
    /// In exclusive mode, STA is disconnected before AP start; this holds the connection
    /// info for restore when the user stops AP.
    stashed_sta_for_restore: RwLock<Option<StashedStaConnection>>,
    /// Whether the most recent exclusive-mode STA restore attempt failed.
    ///
    /// Exposed via `ApStatus.sta_restore_failed` so the frontend can guide the
    /// user when AP stop succeeded but Wi-Fi management-channel recovery did not.
    sta_restore_failed: RwLock<bool>,
    /// Cached AP mode to avoid re-running expensive `detect_capabilities` on every
    /// `ap_status` call. Updated whenever `detect_capabilities` runs.
    cached_ap_mode: RwLock<Option<ApMode>>,
}

impl LinuxNetworkManager {
    /// Create a new instance by connecting to the system D-Bus.
    ///
    /// On startup, attempts to load any persisted STA restore info from a previous
    /// session (covers gateway restart while AP was running in exclusive mode).
    pub async fn new() -> NGResult<Self> {
        let dbus_conn = Connection::system().await.map_err(|e| {
            NetworkError::DBusError(format!("Failed to connect to system D-Bus: {e}"))
        })?;

        let stashed = StashedStaConnection::load().await;
        if stashed.is_some() {
            info!("Loaded persisted STA restore info from previous session");
        }

        info!("Connected to system D-Bus for NetworkManager");
        Ok(Self {
            dbus_conn,
            stashed_sta_for_restore: RwLock::new(stashed),
            sta_restore_failed: RwLock::new(false),
            cached_ap_mode: RwLock::new(None),
        })
    }

    /// Create the NM root proxy.
    async fn nm_proxy(&self) -> NGResult<NetworkManagerProxy<'_>> {
        NetworkManagerProxy::new(&self.dbus_conn)
            .await
            .map_err(|e| NetworkError::NetworkManagerUnavailable(e.to_string()).into())
    }

    /// Create a properties proxy for a given object path.
    async fn props_proxy(&self, path: &ObjectPath<'_>) -> NGResult<NMPropertiesProxy<'_>> {
        NMPropertiesProxy::builder(&self.dbus_conn)
            .path(path.to_owned())
            .map_err(|e| NetworkError::DBusError(format!("Invalid object path: {e}")))?
            .build()
            .await
            .map_err(|e| {
                NetworkError::DBusError(format!("Failed to create props proxy: {e}")).into()
            })
    }

    /// Read a single property via the generic Properties interface.
    async fn get_property(
        &self,
        path: &ObjectPath<'_>,
        iface: &str,
        prop: &str,
    ) -> NGResult<OwnedValue> {
        let proxy = self.props_proxy(path).await?;
        proxy.get(iface, prop).await.map_err(|e| {
            NetworkError::DBusError(format!("Failed to read {iface}.{prop}: {e}")).into()
        })
    }

    /// Read all properties of an interface on an object.
    async fn get_all_properties(
        &self,
        path: &ObjectPath<'_>,
        iface: &str,
    ) -> NGResult<HashMap<String, OwnedValue>> {
        let proxy = self.props_proxy(path).await?;
        proxy.get_all(iface).await.map_err(|e| {
            NetworkError::DBusError(format!("Failed to read all props of {iface}: {e}")).into()
        })
    }

    /// Build a `NetworkInterfaceSummary` from a NM device object path.
    async fn build_interface_summary(
        &self,
        device_path: &OwnedObjectPath,
    ) -> NGResult<Option<NetworkInterfaceSummary>> {
        let device_path_ref = ObjectPath::try_from(device_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;
        let dev_props = self
            .get_all_properties(&device_path_ref, iface::DEVICE)
            .await?;

        let iface_name = prop_str(&dev_props, prop::INTERFACE).unwrap_or_default();
        if iface_name.is_empty() {
            return Ok(None);
        }

        let device_type = prop_u32(&dev_props, prop::DEVICE_TYPE).unwrap_or(0);
        let kind = nm_device_type_to_kind(device_type);

        // Skip loopback and unrecognized virtual interfaces.
        if kind == InterfaceKind::Loopback {
            return Ok(None);
        }

        let state = prop_u32(&dev_props, prop::STATE).unwrap_or(0);
        let link_state = nm_state_to_link_state(state);

        let mac_address = prop_str(&dev_props, prop::HW_ADDRESS);
        let speed_mbps =
            prop_u32(&dev_props, prop::SPEED).and_then(|s| if s > 0 { Some(s) } else { None });
        let _mtu = prop_u32(&dev_props, prop::MTU);
        let _driver = prop_str(&dev_props, prop::DRIVER);

        // IPv4 config
        let ipv4 = if state == device_state::ACTIVATED {
            self.read_ipv4_config(&dev_props).await.ok()
        } else {
            None
        };

        // IPv6 config
        let ipv6 = if state == device_state::ACTIVATED {
            self.read_ipv6_config(&dev_props).await.ok()
        } else {
            None
        };

        // Wi-Fi specific properties
        let (wifi_mode, connected_ssid, ap_ssid, signal_dbm, signal_quality) =
            if kind == InterfaceKind::Wifi && state >= device_state::DISCONNECTED {
                self.read_wifi_info(&device_path_ref, state)
                    .await
                    .unwrap_or_default()
            } else {
                Default::default()
            };

        // Traffic stats from /sys/class/net
        let (rx_bytes, tx_bytes) = read_sysfs_traffic(&iface_name).await;

        Ok(Some(NetworkInterfaceSummary {
            name: iface_name,
            display_name: None,
            kind,
            link_state,
            mac_address,
            ipv4,
            ipv6,
            wifi_mode,
            connected_ssid,
            ap_ssid,
            signal_dbm,
            signal_quality,
            speed_mbps,
            rx_bytes,
            tx_bytes,
        }))
    }

    /// Read IPv4 configuration from the device's Ip4Config object.
    ///
    /// Resolves the actual IP method (DHCP / Static / Disabled) from the active
    /// NM connection profile rather than guessing.
    async fn read_ipv4_config(
        &self,
        dev_props: &HashMap<String, OwnedValue>,
    ) -> NGResult<Ipv4Config> {
        let config_path = prop_object_path(dev_props, prop::IP4_CONFIG)
            .ok_or(NetworkError::ConfigError("No Ip4Config path".to_string()))?;

        if config_path.as_str() == "/" {
            return Err(NetworkError::ConfigError("Ip4Config is /".to_string()).into());
        }

        let config_path_ref = ObjectPath::try_from(config_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid Ip4Config path: {e}")))?;

        let ip4_props = self
            .get_all_properties(&config_path_ref, iface::IP4_CONFIG)
            .await?;

        let addresses = parse_nm_ip4_addresses(&ip4_props);
        let gateway = prop_str(&ip4_props, prop::GATEWAY).and_then(|s| s.parse::<IpAddr>().ok());
        let dns = parse_nm_ip4_nameservers(&ip4_props);
        let search_domains = parse_nm_ip4_search_domains(&ip4_props);

        let method = self.resolve_ip_method(dev_props, conn::IPV4).await;

        Ok(Ipv4Config {
            addresses,
            gateway,
            dns,
            search_domains,
            method,
        })
    }

    /// Read IPv6 configuration from the device's Ip6Config object.
    async fn read_ipv6_config(
        &self,
        dev_props: &HashMap<String, OwnedValue>,
    ) -> NGResult<Ipv6Config> {
        let config_path = prop_object_path(dev_props, prop::IP6_CONFIG)
            .ok_or(NetworkError::ConfigError("No Ip6Config path".to_string()))?;

        if config_path.as_str() == "/" {
            return Err(NetworkError::ConfigError("Ip6Config is /".to_string()).into());
        }

        let config_path_ref = ObjectPath::try_from(config_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid Ip6Config path: {e}")))?;

        let ip6_props = self
            .get_all_properties(&config_path_ref, iface::IP6_CONFIG)
            .await?;

        let addresses = parse_nm_ip6_addresses(&ip6_props);
        let gateway = prop_str(&ip6_props, prop::GATEWAY).and_then(|s| s.parse::<IpAddr>().ok());
        let dns = parse_nm_ip6_nameservers(&ip6_props);

        let method = self.resolve_ip_method(dev_props, conn::IPV6).await;

        Ok(Ipv6Config {
            addresses,
            gateway,
            dns,
            method,
        })
    }

    /// Resolve the actual IP method from the device's active NM connection profile.
    ///
    /// Reads `ActiveConnection` → `Connection` (settings path) → `GetSettings()` →
    /// `ipv4.method` / `ipv6.method` to determine the real method.
    async fn resolve_ip_method(
        &self,
        dev_props: &HashMap<String, OwnedValue>,
        section: &str,
    ) -> IpMethod {
        let active_conn_path = match prop_object_path(dev_props, prop::ACTIVE_CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")
        {
            Some(p) => p,
            None => return IpMethod::Dhcp,
        };

        let active_ref = match ObjectPath::try_from(active_conn_path.as_str()) {
            Ok(p) => p,
            Err(_) => return IpMethod::Dhcp,
        };

        let active_props = self
            .get_all_properties(&active_ref, iface::ACTIVE_CONN)
            .await
            .unwrap_or_default();

        let settings_path = match prop_object_path(&active_props, prop::CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")
        {
            Some(p) => p,
            None => return IpMethod::Dhcp,
        };

        // Call GetSettings() on the connection to read ipv4/ipv6.method.
        let method_str = self
            .dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path.as_str(),
                Some(iface::SETTINGS_CONN),
                dbus_method::GET_SETTINGS,
                &(),
            )
            .await
            .ok()
            .and_then(|reply| {
                let body = reply
                    .body()
                    .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
                    .ok()?;
                let ip_section = body.get(section)?;
                let method_val = ip_section.get(conn::METHOD)?;
                method_val
                    .downcast_ref::<&str>()
                    .ok()
                    .map(|s| s.to_string())
            });

        match method_str.as_deref() {
            Some(method::MANUAL) => IpMethod::Static,
            Some(method::DISABLED) => IpMethod::Disabled,
            _ => IpMethod::Dhcp,
        }
    }

    /// Read Wi-Fi specific information (mode, SSID, signal) for a wireless device.
    async fn read_wifi_info(
        &self,
        device_path: &ObjectPath<'_>,
        device_state: u32,
    ) -> NGResult<(
        Option<WifiMode>,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<u8>,
    )> {
        let wifi_props = self
            .get_all_properties(device_path, iface::DEVICE_WIRELESS)
            .await?;

        let mode = prop_u32(&wifi_props, prop::MODE).map(nm_wifi_mode_to_mode);

        let mut connected_ssid = None;
        let mut signal_dbm = None;
        let mut signal_quality = None;

        if device_state == device_state::ACTIVATED {
            if let Some(active_ap_path) = prop_object_path(&wifi_props, prop::ACTIVE_ACCESS_POINT) {
                if active_ap_path.as_str() != "/" {
                    let ap_path_ref = ObjectPath::try_from(active_ap_path.as_str())
                        .map_err(|e| NetworkError::DBusError(format!("Invalid AP path: {e}")))?;
                    let ap_props = self
                        .get_all_properties(&ap_path_ref, iface::ACCESS_POINT)
                        .await?;

                    connected_ssid = prop_byte_array(&ap_props, prop::SSID)
                        .map(|bytes| String::from_utf8_lossy(&bytes).to_string());

                    let strength = prop_u8(&ap_props, prop::STRENGTH).unwrap_or(0);
                    signal_quality = Some(strength);
                    signal_dbm = Some(quality_to_rssi(strength));
                }
            }
        }

        Ok((mode, connected_ssid, None, signal_dbm, signal_quality))
    }

    /// Find the first wireless device's object path.
    async fn find_wireless_device(
        &self,
        interface_name: Option<&str>,
    ) -> NGResult<OwnedObjectPath> {
        let nm = self.nm_proxy().await?;
        let devices = nm
            .get_devices()
            .await
            .map_err(|e| NetworkError::DBusError(format!("GetDevices failed: {e}")))?;

        for dev_path in &devices {
            let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;
            let dev_props = self
                .get_all_properties(&dev_path_ref, iface::DEVICE)
                .await?;

            let device_type = prop_u32(&dev_props, prop::DEVICE_TYPE).unwrap_or(0);
            if device_type != device_type::WIFI {
                continue;
            }

            if let Some(target) = interface_name {
                let name = prop_str(&dev_props, prop::INTERFACE).unwrap_or_default();
                if name != target {
                    continue;
                }
            }

            return Ok(dev_path.clone());
        }

        Err(
            NetworkError::InterfaceNotFound(interface_name.unwrap_or("(any wireless)").to_string())
                .into(),
        )
    }

    /// Clean up a failed/timed-out Wi-Fi connection attempt.
    ///
    /// Deactivates the active connection and deletes the settings profile so NM
    /// doesn't accumulate orphaned entries.
    async fn cleanup_connection_profile(
        &self,
        active_conn_path: &OwnedObjectPath,
        settings_path: &OwnedObjectPath,
    ) {
        // Deactivate the active connection (best-effort).
        if let Ok(nm) = self.nm_proxy().await {
            if let Ok(active_ref) = ObjectPath::try_from(active_conn_path.as_str()) {
                let _ = nm.deactivate_connection(&active_ref).await;
            }
        }

        // Delete the connection profile via Settings.Connection.Delete (best-effort).
        let _ = self
            .dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path.as_str(),
                Some(iface::SETTINGS_CONN),
                dbus_method::DELETE,
                &(),
            )
            .await;
    }

    /// Find an existing NM connection profile (settings path) for the given interface.
    ///
    /// Two-tier lookup:
    /// 1. **Active connection** — fast O(1) read from the device's `ActiveConnection`
    ///    D-Bus property. Sufficient when the device is in `ACTIVATED` state.
    /// 2. **Saved profiles** — enumerate `Settings.ListConnections`, filter by
    ///    `connection.interface-name`, and pick the most recently used profile
    ///    (highest `connection.timestamp`). Handles transient NM states where the
    ///    active connection object is unavailable (DHCP renegotiation, state
    ///    transitions, etc.).
    async fn find_connection_for_interface(&self, iface_name: &str) -> Option<String> {
        if let Some(path) = self.find_active_connection_for_device(iface_name).await {
            return Some(path);
        }

        debug!(
            interface = iface_name,
            "No active connection, searching saved profiles"
        );
        self.find_saved_connection_for_interface(iface_name).await
    }

    /// Tier-1: resolve settings path from the device's current active connection.
    async fn find_active_connection_for_device(&self, iface_name: &str) -> Option<String> {
        let nm = self.nm_proxy().await.ok()?;
        let device_path = nm.get_device_by_ip_iface(iface_name).await.ok()?;
        let dev_path_ref = ObjectPath::try_from(device_path.as_str()).ok()?;
        let dev_props = self
            .get_all_properties(&dev_path_ref, iface::DEVICE)
            .await
            .ok()?;

        let active_conn_path = prop_object_path(&dev_props, prop::ACTIVE_CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")?;

        let active_ref = ObjectPath::try_from(active_conn_path.as_str()).ok()?;
        let active_props = self
            .get_all_properties(&active_ref, iface::ACTIVE_CONN)
            .await
            .ok()?;

        prop_object_path(&active_props, prop::CONNECTION).filter(|p| !p.is_empty() && p != "/")
    }

    /// Tier-2: search all saved NM profiles by `connection.interface-name`.
    ///
    /// When multiple profiles match (e.g. a WiFi interface that has connected to
    /// several networks), picks the one with the highest `connection.timestamp`
    /// (most recently used).
    async fn find_saved_connection_for_interface(&self, iface_name: &str) -> Option<String> {
        let conn_paths: Vec<OwnedObjectPath> = self
            .dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path::ROOT,
                Some(iface::SETTINGS),
                dbus_method::LIST_CONNECTIONS,
                &(),
            )
            .await
            .ok()?
            .body()
            .deserialize::<Vec<OwnedObjectPath>>()
            .ok()?;

        let mut best: Option<(String, u64)> = None;

        for conn_path in &conn_paths {
            let conn_ref = match ObjectPath::try_from(conn_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let settings = match self.get_connection_settings(&conn_ref).await {
                Some(s) => s,
                None => continue,
            };

            let conn_section = match settings.get(conn::CONNECTION) {
                Some(s) => s,
                None => continue,
            };

            if settings_str(conn_section, conn::INTERFACE_NAME).as_deref() != Some(iface_name) {
                continue;
            }

            let ts = settings_u64(conn_section, conn::TIMESTAMP).unwrap_or(0);
            if best.as_ref().is_none_or(|(_, prev_ts)| ts > *prev_ts) {
                best = Some((conn_path.to_string(), ts));
            }
        }

        if let Some((ref path, _)) = best {
            debug!(
                interface = iface_name,
                path = %path,
                "Found saved connection profile"
            );
        }

        best.map(|(path, _)| path)
    }

    /// Stash the current active STA connection for later restore (exclusive mode only).
    ///
    /// Reads the ActiveConnection from the Wi-Fi device, then the Connection property
    /// (settings path) from that ActiveConnection. Also captures:
    /// - The stable NM profile UUID (survives NM restarts / object-path renumbering).
    /// - A lightweight IPv4 fingerprint from the profile for post-restore verification.
    ///
    /// Returns `None` if no active connection.
    async fn stash_active_sta_connection(&self) -> Option<StashedStaConnection> {
        let dev_path = self.find_wireless_device(None).await.ok()?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str()).ok()?;
        let dev_props = self
            .get_all_properties(&dev_path_ref, iface::DEVICE)
            .await
            .ok()?;

        let active_conn_path = prop_object_path(&dev_props, prop::ACTIVE_CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")?;

        let active_conn_ref = ObjectPath::try_from(active_conn_path.as_str()).ok()?;
        let active_props = self
            .get_all_properties(&active_conn_ref, iface::ACTIVE_CONN)
            .await
            .ok()?;

        let connection_path = prop_object_path(&active_props, prop::CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")?;

        // Capture the stable UUID from the active connection properties.
        let connection_uuid = prop_str(&active_props, prop::UUID);

        // Read the NM profile's ipv4 section to build a verification fingerprint.
        let ipv4_fingerprint = self.read_ipv4_fingerprint(&connection_path).await;

        if let Some(ref fp) = ipv4_fingerprint {
            info!(
                method = %fp.method,
                address = ?fp.address,
                uuid = ?connection_uuid,
                "Stashed STA connection with IPv4 fingerprint"
            );
        }

        Some(StashedStaConnection {
            connection_path,
            device_path: dev_path.to_string(),
            connection_uuid,
            ipv4_fingerprint,
        })
    }

    /// Read the IPv4 fingerprint from an NM connection profile (settings path).
    ///
    /// This reads the persisted profile via `GetSettings()`, not the runtime IP
    /// configuration — so it reflects the user's intended configuration rather
    /// than whatever the kernel currently has assigned.
    async fn read_ipv4_fingerprint(&self, settings_path: &str) -> Option<Ipv4Fingerprint> {
        let conn_ref = ObjectPath::try_from(settings_path).ok()?;
        let settings = self.get_connection_settings(&conn_ref).await?;
        let ipv4_section = settings.get(conn::IPV4)?;

        let method = settings_str(ipv4_section, conn::METHOD).unwrap_or(method::AUTO.to_string());

        let (address, prefix, gateway) = if method == method::MANUAL {
            let addr_info = parse_settings_address_data(ipv4_section);
            let gw = settings_str(ipv4_section, conn::GATEWAY);
            match addr_info {
                Some((ip, pfx)) => (Some(ip.to_string()), Some(pfx), gw),
                None => (None, None, gw),
            }
        } else {
            (None, None, None)
        };

        Some(Ipv4Fingerprint {
            method,
            address,
            prefix,
            gateway,
        })
    }

    /// Temporarily disable autoconnect on competing Wi-Fi profiles for an interface.
    ///
    /// This narrows the remaining race window after `managed yes`: even if
    /// NetworkManager notices the device before our explicit `ActivateConnection`,
    /// competing profiles on the same interface are prevented from opportunistically
    /// claiming the device.
    ///
    /// Returns the list of profiles whose autoconnect state was modified, so the
    /// caller can restore them after the restore transaction finishes.
    async fn suppress_competing_wifi_autoconnect(
        &self,
        iface_name: &str,
        stashed: &StashedStaConnection,
    ) -> Vec<AutoconnectSuppression> {
        let conn_paths: Vec<OwnedObjectPath> = match self
            .dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path::ROOT,
                Some(iface::SETTINGS),
                dbus_method::LIST_CONNECTIONS,
                &(),
            )
            .await
            .ok()
            .and_then(|reply| reply.body().deserialize::<Vec<OwnedObjectPath>>().ok())
        {
            Some(paths) => paths,
            None => return Vec::new(),
        };

        let mut suppressed = Vec::new();

        for conn_path in &conn_paths {
            let conn_ref = match ObjectPath::try_from(conn_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let settings = match self.get_connection_settings(&conn_ref).await {
                Some(s) => s,
                None => continue,
            };
            let conn_section = match settings.get(conn::CONNECTION) {
                Some(s) => s,
                None => continue,
            };

            if settings_str(conn_section, conn::TYPE).as_deref() != Some(conn::WIFI) {
                continue;
            }

            let settings_path = conn_path.to_string();
            let conn_uuid = settings_str(conn_section, conn::UUID);

            if settings_path == stashed.connection_path
                || conn_uuid.as_deref() == stashed.connection_uuid.as_deref()
            {
                continue;
            }

            let profile_iface = settings_str(conn_section, conn::INTERFACE_NAME);
            if profile_iface
                .as_deref()
                .is_some_and(|name| name != iface_name)
            {
                continue;
            }

            let autoconnect = settings_bool(conn_section, conn::AUTOCONNECT).unwrap_or(true);
            if !autoconnect {
                continue;
            }

            info!(
                path = %settings_path,
                uuid = ?conn_uuid,
                interface = iface_name,
                "Temporarily disabling competing Wi-Fi profile autoconnect during STA restore"
            );
            self.set_connection_autoconnect(&settings_path, false).await;
            suppressed.push(AutoconnectSuppression {
                settings_path,
                previous_autoconnect: true,
            });
        }

        suppressed
    }

    /// Restore autoconnect flags previously suppressed during STA restore.
    async fn restore_autoconnect_suppressions(&self, suppressed: &[AutoconnectSuppression]) {
        for entry in suppressed {
            self.set_connection_autoconnect(&entry.settings_path, entry.previous_autoconnect)
                .await;
        }
    }

    /// Attempt to restore AP on a virtual interface after STA connection succeeds.
    ///
    /// This transitions the AP from exclusive mode to concurrent mode by:
    /// 1. Updating `ap-env` to use a virtual AP interface (`{sta_iface}_ap`)
    /// 2. Setting `AP_EXCLUSIVE=false`
    /// 3. Restarting the AP service stack
    async fn try_restore_ap_concurrent(&self) -> NGResult<()> {
        let env_path = format!("{}/{}", AP_CONFIG_DIR, AP_ENV_FILE);
        let current_env = tokio::fs::read_to_string(&env_path)
            .await
            .unwrap_or_default();

        let sta_iface = parse_env_value(&current_env, "STA_IFACE").unwrap_or("wlan0".to_string());
        let ap_iface = format!("{sta_iface}_ap");

        let updated_env = current_env
            .lines()
            .map(|line| {
                if line.starts_with("AP_IFACE=") {
                    format!("AP_IFACE=\"{ap_iface}\"")
                } else if line.starts_with("AP_EXCLUSIVE=") {
                    "AP_EXCLUSIVE=\"false\"".to_string()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        tokio::fs::write(&env_path, &updated_env)
            .await
            .map_err(|e| {
                NetworkError::ApError(format!("Failed to update ap-env for concurrent mode: {e}"))
            })?;

        // Update hostapd.conf and dnsmasq-ap.conf interface lines.
        for filename in [HOSTAPD_CONF_FILE, ap_config::DNSMASQ_AP_CONF_FILE] {
            let path = format!("{}/{}", AP_CONFIG_DIR, filename);
            if let Ok(content) = tokio::fs::read_to_string(&path).await {
                let updated = content
                    .lines()
                    .map(|line| {
                        if line.starts_with("interface=") {
                            format!("interface={ap_iface}")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = tokio::fs::write(&path, &updated).await;
            }
        }

        info!(
            ap_iface = %ap_iface,
            "Updated AP config for concurrent mode — restarting AP services"
        );

        let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
        mgr.start_all().await?;

        // Update cached AP mode since we've transitioned to concurrent.
        let mut cached = self.cached_ap_mode.write().await;
        *cached = Some(ApMode::Concurrent);

        info!("AP restored on virtual interface in concurrent mode");
        Ok(())
    }

    /// Restore a previously stashed STA connection after stopping (or failing to start) AP.
    ///
    /// Orchestrates a controlled restore sequence that avoids the NM autoconnect race:
    ///
    /// 1. Run `ap-teardown.sh` with `DEFER_NM_HANDOFF=true` — this does L2/L3
    ///    cleanup (type change, IP flush, link up) but does NOT hand the interface
    ///    back to NetworkManager.
    /// 2. Explicitly hand the interface to NM (`nmcli device set managed yes`).
    /// 3. Wait for NM to discover the interface.
    /// 4. Call `ActivateConnection` with the stashed profile path (fallback to UUID).
    /// 5. Wait for the connection to reach `ACTIVATED` state.
    /// 6. Verify that the active connection's IPv4 configuration matches the
    ///    stashed fingerprint. If mismatched (e.g. NM somehow used a different
    ///    profile or the profile was reverted to DHCP), attempt corrective
    ///    re-activation.
    ///
    /// Called from both `stop_ap` and the `start_ap` failure rollback path.
    async fn restore_stashed_sta_connection(&self) {
        let stashed = self.stashed_sta_for_restore.read().await.clone();
        let Some(s) = stashed else { return };

        info!(
            connection_path = %s.connection_path,
            uuid = ?s.connection_uuid,
            expected_ipv4 = ?s.ipv4_fingerprint,
            "Restoring stashed STA connection"
        );

        // ── Step 1: L2/L3 teardown (NM handoff deferred) ──
        let env_path = format!("{}/{}", AP_CONFIG_DIR, AP_ENV_FILE);
        let env_content = tokio::fs::read_to_string(&env_path)
            .await
            .unwrap_or_default();
        let sta_iface = parse_env_value(&env_content, "STA_IFACE").unwrap_or_default();

        let suppressed_profiles = if !sta_iface.is_empty() {
            self.suppress_competing_wifi_autoconnect(&sta_iface, &s)
                .await
        } else {
            Vec::new()
        };

        if !sta_iface.is_empty() {
            let mut cmd = tokio::process::Command::new("/opt/ng-gateway/scripts/ap-teardown.sh");
            for line in env_content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, val)) = line.split_once('=') {
                    let val = val.trim_matches('"');
                    cmd.env(key, val);
                }
            }
            // Force deferred handoff regardless of what ap-env currently says,
            // because the Rust process will manage NM re-attachment below.
            cmd.env("DEFER_NM_HANDOFF", "true");
            let _ = cmd.output().await;
        }

        // ── Step 2: Hand interface to NM and activate atomically ──
        //
        // To eliminate the autoconnect race, we:
        //   a) Set the device managed
        //   b) Poll device state until NM transitions it to at least DISCONNECTED
        //   c) Immediately fire ActivateConnection — no fixed sleep gap for NM
        //      to opportunistically autoconnect a different profile
        let activate_result = if !sta_iface.is_empty() {
            let _ = tokio::process::Command::new("nmcli")
                .args(["device", "set", &sta_iface, "managed", "yes"])
                .output()
                .await;

            // Poll until NM recognizes the device (state ≥ DISCONNECTED).
            self.poll_device_ready(&sta_iface, Duration::from_secs(5))
                .await;

            self.activate_stashed_connection(&s).await
        } else {
            Err("No STA interface name available".into())
        };

        match activate_result {
            Ok(()) => {
                info!("STA connection activation requested — waiting for connection");

                // Poll until the connection reaches ACTIVATED state (DHCP lease
                // acquired or static IP assigned) rather than a fixed sleep.
                let activated = self
                    .poll_connection_activated(&sta_iface, Duration::from_secs(15))
                    .await;

                if !activated {
                    warn!("Connection did not reach ACTIVATED state within timeout");
                }

                // ── Step 4: Post-restore verification ──
                let verified = self.verify_sta_restore(&s, &sta_iface).await;

                if activated && verified {
                    {
                        let mut guard = self.stashed_sta_for_restore.write().await;
                        *guard = None;
                    }
                    StashedStaConnection::remove().await;

                    let mut restore_failed = self.sta_restore_failed.write().await;
                    *restore_failed = false;
                } else {
                    let mut restore_failed = self.sta_restore_failed.write().await;
                    *restore_failed = true;
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    uuid = ?s.connection_uuid,
                    "Failed to restore STA connection — device may need manual WiFi reconnection"
                );
                let mut restore_failed = self.sta_restore_failed.write().await;
                *restore_failed = true;
            }
        }

        self.restore_autoconnect_suppressions(&suppressed_profiles)
            .await;
    }

    /// Attempt to activate the stashed NM connection profile.
    ///
    /// Tries `connection_path` first; if that fails (stale object path after NM
    /// restart), falls back to resolving the profile by UUID.
    async fn activate_stashed_connection(&self, s: &StashedStaConnection) -> Result<(), String> {
        let dev_ref = ObjectPath::try_from(s.device_path.as_str())
            .map_err(|e| format!("Invalid device path: {e}"))?;
        let root = ObjectPath::try_from("/").map_err(|e| format!("Invalid root path: {e}"))?;
        let nm = self
            .nm_proxy()
            .await
            .map_err(|e| format!("NM proxy: {e}"))?;

        // Primary attempt: use the stashed connection_path directly.
        let conn_ref = ObjectPath::try_from(s.connection_path.as_str());
        if let Ok(ref conn_path) = conn_ref {
            match nm.activate_connection(conn_path, &dev_ref, &root).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    warn!(
                        error = %e,
                        path = %s.connection_path,
                        "ActivateConnection via stashed path failed — trying UUID fallback"
                    );
                }
            }
        }

        // Fallback: resolve by UUID (survives NM restart / path renumbering).
        let uuid = s
            .connection_uuid
            .as_deref()
            .ok_or("No UUID available for fallback")?;

        let resolved_path = self
            .find_saved_connection_by_uuid(uuid)
            .await
            .ok_or(format!("No saved profile found for UUID {uuid}"))?;

        let resolved_ref = ObjectPath::try_from(resolved_path.as_str())
            .map_err(|e| format!("Invalid resolved path: {e}"))?;

        nm.activate_connection(&resolved_ref, &dev_ref, &root)
            .await
            .map_err(|e| format!("ActivateConnection via UUID fallback: {e}"))?;

        info!(uuid = uuid, "Restored STA via UUID fallback");
        Ok(())
    }

    /// Find a saved NM connection profile by UUID.
    async fn find_saved_connection_by_uuid(&self, target_uuid: &str) -> Option<String> {
        let conn_paths: Vec<OwnedObjectPath> = self
            .dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path::ROOT,
                Some(iface::SETTINGS),
                dbus_method::LIST_CONNECTIONS,
                &(),
            )
            .await
            .ok()?
            .body()
            .deserialize::<Vec<OwnedObjectPath>>()
            .ok()?;

        for conn_path in &conn_paths {
            let conn_ref = match ObjectPath::try_from(conn_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let settings = match self.get_connection_settings(&conn_ref).await {
                Some(s) => s,
                None => continue,
            };
            let conn_section = match settings.get(conn::CONNECTION) {
                Some(s) => s,
                None => continue,
            };
            if settings_str(conn_section, conn::UUID).as_deref() == Some(target_uuid) {
                return Some(conn_path.to_string());
            }
        }
        None
    }

    /// Verify that the restored STA connection matches the stashed IPv4 fingerprint.
    ///
    /// If a mismatch is detected (wrong profile activated, or profile IPv4 settings
    /// reverted), logs a detailed warning with both expected and actual values and
    /// returns whether the restore outcome is acceptable.
    async fn verify_sta_restore(&self, stashed: &StashedStaConnection, sta_iface: &str) -> bool {
        let expected_fp = match &stashed.ipv4_fingerprint {
            Some(fp) => fp,
            None => {
                debug!("No IPv4 fingerprint stashed — skipping post-restore verification");
                return true;
            }
        };

        // Read the currently active connection's profile to compare.
        let actual_fp = match self.read_active_ipv4_fingerprint(sta_iface).await {
            Some(fp) => fp,
            None => {
                warn!(
                    interface = sta_iface,
                    "Cannot read active connection IPv4 config — post-restore verification skipped"
                );
                return false;
            }
        };

        if *expected_fp == actual_fp {
            info!(
                method = %actual_fp.method,
                address = ?actual_fp.address,
                "Post-restore verification passed — IPv4 config matches stashed fingerprint"
            );
            return true;
        }

        // Mismatch detected — log details for diagnostics.
        warn!(
            expected_method = %expected_fp.method,
            expected_address = ?expected_fp.address,
            expected_gateway = ?expected_fp.gateway,
            actual_method = %actual_fp.method,
            actual_address = ?actual_fp.address,
            actual_gateway = ?actual_fp.gateway,
            "Post-restore IPv4 mismatch — active connection does not match stashed profile"
        );

        // Attempt corrective re-activation: resolve the profile (with UUID fallback)
        // to confirm its IPv4 settings are still intact, then force re-activate.
        let profile_fp = self.resolve_profile_fingerprint(stashed).await;
        if profile_fp.as_ref() == Some(expected_fp) {
            info!("Stashed NM profile still has correct IPv4 settings — attempting corrective re-activation");
            if let Ok(()) = self.activate_stashed_connection(stashed).await {
                self.poll_connection_activated(sta_iface, Duration::from_secs(15))
                    .await;
                if let Some(recheck) = self.read_active_ipv4_fingerprint(sta_iface).await {
                    if recheck == *expected_fp {
                        info!("Corrective re-activation succeeded — IPv4 config restored");
                        return true;
                    }
                    warn!(
                        actual_method = %recheck.method,
                        actual_address = ?recheck.address,
                        "Corrective re-activation did not fix the mismatch"
                    );
                }
            }
        } else {
            warn!(
                profile_fp = ?profile_fp,
                "NM profile IPv4 settings differ from stashed fingerprint — profile may have been modified externally"
            );
        }

        false
    }

    /// Resolve the IPv4 fingerprint of a stashed profile, with UUID fallback.
    ///
    /// First tries the stashed `connection_path`. If that fails (e.g. NM restarted
    /// and object paths were renumbered), falls back to locating the profile by UUID
    /// and reading its fingerprint. This keeps the verification path consistent with
    /// the activation path that already supports UUID fallback.
    async fn resolve_profile_fingerprint(
        &self,
        stashed: &StashedStaConnection,
    ) -> Option<Ipv4Fingerprint> {
        if let Some(fp) = self.read_ipv4_fingerprint(&stashed.connection_path).await {
            return Some(fp);
        }

        // connection_path stale — try UUID fallback.
        let uuid = stashed.connection_uuid.as_deref()?;
        let resolved_path = self.find_saved_connection_by_uuid(uuid).await?;
        debug!(uuid = uuid, path = %resolved_path, "Resolved profile via UUID for fingerprint check");
        self.read_ipv4_fingerprint(&resolved_path).await
    }

    /// Read the IPv4 fingerprint of the currently active connection on a given interface.
    ///
    /// This reads the persisted NM profile (via `GetSettings`) of whatever connection
    /// is currently active on the device, giving us the *configured* method rather
    /// than the runtime IP state.
    async fn read_active_ipv4_fingerprint(&self, iface_name: &str) -> Option<Ipv4Fingerprint> {
        let settings_path = self.find_active_connection_for_device(iface_name).await?;
        self.read_ipv4_fingerprint(&settings_path).await
    }

    /// Poll until NM recognizes a device and it reaches at least `DISCONNECTED` state.
    ///
    /// After `nmcli device set <iface> managed yes`, NM needs a moment to transition
    /// the device from `UNMANAGED` → `UNAVAILABLE` → `DISCONNECTED`. We poll with
    /// exponential backoff (100ms → 200ms → 400ms, capped at 500ms) instead of a
    /// fixed sleep, and fire `ActivateConnection` the instant the device is ready.
    /// This eliminates the autoconnect window that a fixed sleep would create.
    async fn poll_device_ready(&self, iface_name: &str, timeout: Duration) {
        let start = Instant::now();
        let mut interval = Duration::from_millis(100);

        loop {
            if let Some(state) = self.read_device_state(iface_name).await {
                if state >= device_state::DISCONNECTED {
                    debug!(
                        iface = iface_name,
                        state = state,
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        "Device reached ready state"
                    );
                    return;
                }
            }

            if start.elapsed() >= timeout {
                warn!(
                    iface = iface_name,
                    "Timeout waiting for device to become ready — proceeding anyway"
                );
                return;
            }

            sleep(interval).await;
            interval = (interval * 2).min(Duration::from_millis(500));
        }
    }

    /// Poll until the active connection on a device reaches `ACTIVATED` state.
    ///
    /// Used after `ActivateConnection` to wait for the connection to fully establish
    /// (DHCP lease acquired or static IP assigned) before running post-restore
    /// verification.
    async fn poll_connection_activated(&self, iface_name: &str, timeout: Duration) -> bool {
        let start = Instant::now();
        let mut interval = Duration::from_millis(200);

        loop {
            if let Some(state) = self.read_device_state(iface_name).await {
                if state == device_state::ACTIVATED {
                    debug!(
                        iface = iface_name,
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        "Connection reached ACTIVATED state"
                    );
                    return true;
                }
            }

            if start.elapsed() >= timeout {
                warn!(
                    iface = iface_name,
                    "Timeout waiting for connection to activate"
                );
                return false;
            }

            sleep(interval).await;
            interval = (interval * 2).min(Duration::from_millis(1000));
        }
    }

    /// Read the NM device state for an interface by name.
    async fn read_device_state(&self, iface_name: &str) -> Option<u32> {
        let nm = self.nm_proxy().await.ok()?;
        let dev_path = nm.get_device_by_ip_iface(iface_name).await.ok()?;
        let dev_ref = ObjectPath::try_from(dev_path.as_str()).ok()?;
        let props = self
            .get_all_properties(&dev_ref, iface::DEVICE)
            .await
            .ok()?;
        prop_u32(&props, prop::STATE)
    }

    /// Force-restore the WiFi interface to managed mode after a failed AP start.
    ///
    /// When `ap-setup.sh` runs in exclusive mode, it converts the interface to
    /// `__ap` type and removes it from NM control. If hostapd then fails to start,
    /// the interface is stuck in `__ap` mode and the device is unreachable.
    ///
    /// This method only performs L2/L3 cleanup (down → managed type → flush →
    /// up). NM ownership handoff is intentionally omitted — the caller is
    /// expected to invoke `restore_stashed_sta_connection` next, which handles
    /// NM re-attachment and explicit `ActivateConnection` in a race-free sequence.
    async fn force_restore_wifi_interface(&self) {
        let env_path = format!("{}/{}", AP_CONFIG_DIR, AP_ENV_FILE);
        let env_content = tokio::fs::read_to_string(&env_path)
            .await
            .unwrap_or_default();

        let ap_iface = parse_env_value(&env_content, "AP_IFACE").unwrap_or_default();
        if ap_iface.is_empty() {
            warn!("Cannot restore interface: AP_IFACE not found in ap-env");
            return;
        }

        info!(iface = %ap_iface, "Force-restoring WiFi interface to managed mode (L2/L3 only)");

        let commands: &[&[&str]] = &[
            &["ip", "link", "set", &ap_iface, "down"],
            &["iw", "dev", &ap_iface, "set", "type", "managed"],
            &["ip", "addr", "flush", "dev", &ap_iface],
            &["ip", "link", "set", &ap_iface, "up"],
        ];

        for cmd in commands {
            let result = tokio::process::Command::new(cmd[0])
                .args(&cmd[1..])
                .output()
                .await;
            if let Err(e) = result {
                warn!(cmd = ?cmd, error = %e, "Force-restore command failed");
            }
        }

        info!(iface = %ap_iface, "WiFi interface restored to managed mode (NM handoff deferred to restore_stashed_sta_connection)");
    }

    /// Core Wi-Fi disconnect logic without AP restore evaluation.
    ///
    /// Separated from the trait method so that internal callers (e.g. `start_ap`)
    /// can disconnect STA without triggering `evaluate_and_restore_ap`, which would
    /// race with the AP startup sequence.
    async fn disconnect_wifi_inner(
        &self,
        interface_name: Option<&str>,
        disable_autoconnect: bool,
    ) -> NGResult<()> {
        let dev_path = self.find_wireless_device(interface_name).await?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        let dev_props = self
            .get_all_properties(&dev_path_ref, iface::DEVICE)
            .await?;

        let active_conn =
            prop_object_path(&dev_props, prop::ACTIVE_CONNECTION).filter(|p| p != "/");

        if let Some(conn_path) = &active_conn {
            let settings_path = {
                let active_ref = ObjectPath::try_from(conn_path.as_str()).ok();
                if let Some(ref active_ref) = active_ref {
                    let active_props = self
                        .get_all_properties(active_ref, iface::ACTIVE_CONN)
                        .await
                        .ok();
                    active_props.and_then(|p| {
                        prop_object_path(&p, prop::CONNECTION).filter(|s| !s.is_empty() && s != "/")
                    })
                } else {
                    None
                }
            };

            let nm = self.nm_proxy().await?;
            let conn_path_ref = ObjectPath::try_from(conn_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid connection path: {e}")))?;
            nm.deactivate_connection(&conn_path_ref)
                .await
                .map_err(|e| NetworkError::WifiError(format!("Failed to deactivate: {e}")))?;
            info!("Wi-Fi disconnected");

            if disable_autoconnect {
                if let Some(ref sp) = settings_path {
                    self.set_connection_autoconnect(sp, false).await;
                }
            }
        } else {
            debug!("No active Wi-Fi connection to disconnect");
        }

        Ok(())
    }

    /// Update the `autoconnect` flag on a saved NM connection profile.
    ///
    /// Reads current settings via `GetSettings`, modifies `connection.autoconnect`,
    /// then writes back via `Update`.
    async fn set_connection_autoconnect(&self, settings_path: &str, autoconnect: bool) {
        let current = self
            .dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path,
                Some(iface::SETTINGS_CONN),
                dbus_method::GET_SETTINGS,
                &(),
            )
            .await
            .ok()
            .and_then(|reply| {
                reply
                    .body()
                    .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
                    .ok()
            });

        let Some(mut current) = current else {
            warn!("Failed to read connection settings for autoconnect update");
            return;
        };

        let conn_section = current.entry(conn::CONNECTION.to_string()).or_default();
        conn_section.insert(conn::AUTOCONNECT.to_string(), OwnedValue::from(autoconnect));

        if let Err(e) = self
            .dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path,
                Some(iface::SETTINGS_CONN),
                dbus_method::UPDATE,
                &(current,),
            )
            .await
        {
            warn!(error = %e, "Failed to update autoconnect flag");
        } else {
            debug!(
                autoconnect = autoconnect,
                path = settings_path,
                "Updated connection autoconnect"
            );
        }
    }

    /// Evaluate whether AP hotspot should be restored after STA disconnection.
    ///
    /// Restores AP unconditionally as long as hardware supports it and no Wi-Fi
    /// STA connection is active. This ensures the gateway always has a reachable
    /// management channel when possible.
    async fn evaluate_and_restore_ap(&self) {
        let ap_mode = {
            let cached = self.cached_ap_mode.read().await;
            cached.unwrap_or(ApMode::Unavailable)
        };

        if matches!(ap_mode, ApMode::Unavailable) {
            return;
        }

        if self.has_any_active_sta().await {
            debug!("STA still connected — AP restore not needed");
            return;
        }

        let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
        if let Ok(status) = mgr.status().await {
            if status.ap_broadcasting() {
                debug!("AP already broadcasting — no restore needed");
                return;
            }
        }

        info!("No active STA connection — restoring AP hotspot");

        if let Ok(caps) = self.detect_capabilities().await {
            if let Err(e) = self.sync_ap_config_with_mode(&caps).await {
                warn!(error = %e, "Failed to sync AP config before restore");
            }
        }

        if let Err(e) = mgr.start_all().await {
            tracing::error!(error = %e, "CRITICAL: Failed to restore AP after WiFi disconnect");
        } else {
            info!("AP hotspot restored as fallback management channel");
        }
    }

    /// Check if any Wi-Fi STA connection is currently active across all wireless devices.
    async fn has_any_active_sta(&self) -> bool {
        let nm = match self.nm_proxy().await {
            Ok(nm) => nm,
            Err(_) => return false,
        };

        let devices = nm.get_devices().await.unwrap_or_default();
        for dev_path in &devices {
            let dev_path_ref = match ObjectPath::try_from(dev_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let dev_props = self
                .get_all_properties(&dev_path_ref, iface::DEVICE)
                .await
                .unwrap_or_default();

            if prop_u32(&dev_props, prop::DEVICE_TYPE) != Some(device_type::WIFI) {
                continue;
            }

            let state = prop_u32(&dev_props, prop::STATE).unwrap_or(0);
            if state == device_state::ACTIVATED {
                let wifi_props = self
                    .get_all_properties(&dev_path_ref, iface::DEVICE_WIRELESS)
                    .await
                    .unwrap_or_default();
                let mode = prop_u32(&wifi_props, prop::MODE).unwrap_or(0);
                // NM_802_11_MODE_INFRA = 2 (STA mode)
                if mode == 2 {
                    return true;
                }
            }
        }
        false
    }

    /// Collect UUIDs of all currently active NM connections.
    async fn collect_active_connection_uuids(&self) -> HashSet<String> {
        let mut uuids = HashSet::new();
        let nm = match self.nm_proxy().await {
            Ok(nm) => nm,
            Err(_) => return uuids,
        };

        let devices = nm.get_devices().await.unwrap_or_default();
        for dev_path in &devices {
            let dev_path_ref = match ObjectPath::try_from(dev_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let dev_props = self
                .get_all_properties(&dev_path_ref, iface::DEVICE)
                .await
                .unwrap_or_default();

            let active_conn_path = prop_object_path(&dev_props, prop::ACTIVE_CONNECTION)
                .filter(|p| !p.is_empty() && p != "/");

            if let Some(active_path) = active_conn_path {
                let active_ref = match ObjectPath::try_from(active_path.as_str()) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let active_props = self
                    .get_all_properties(&active_ref, iface::ACTIVE_CONN)
                    .await
                    .unwrap_or_default();

                if let Some(uuid) = prop_str(&active_props, prop::UUID) {
                    uuids.insert(uuid);
                }
            }
        }
        uuids
    }

    /// Read NM connection settings via `GetSettings()`.
    async fn get_connection_settings(
        &self,
        conn_path: &ObjectPath<'_>,
    ) -> Option<HashMap<String, HashMap<String, OwnedValue>>> {
        self.dbus_conn
            .call_method(
                Some(iface::NM),
                conn_path.as_str(),
                Some(iface::SETTINGS_CONN),
                dbus_method::GET_SETTINGS,
                &(),
            )
            .await
            .ok()
            .and_then(|reply| {
                reply
                    .body()
                    .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
                    .ok()
            })
    }

    /// Find the active connection path for a given UUID.
    async fn find_active_connection_by_uuid(&self, uuid: &str) -> Option<String> {
        let nm = self.nm_proxy().await.ok()?;
        let devices = nm.get_devices().await.unwrap_or_default();

        for dev_path in &devices {
            let dev_path_ref = match ObjectPath::try_from(dev_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let dev_props = self
                .get_all_properties(&dev_path_ref, iface::DEVICE)
                .await
                .unwrap_or_default();

            let active_conn_path = prop_object_path(&dev_props, prop::ACTIVE_CONNECTION)
                .filter(|p| !p.is_empty() && p != "/");

            if let Some(ref active_path) = active_conn_path {
                let active_ref = match ObjectPath::try_from(active_path.as_str()) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let active_props = self
                    .get_all_properties(&active_ref, iface::ACTIVE_CONN)
                    .await
                    .unwrap_or_default();

                if prop_str(&active_props, prop::UUID).as_deref() == Some(uuid) {
                    return Some(active_path.clone());
                }
            }
        }
        None
    }

    /// Ensure `ap-env` and `hostapd.conf` reflect the runtime-detected AP mode
    /// and that the hostapd channel is actually usable (non-DFS, non-disabled).
    ///
    /// Handles three classes of drift:
    /// 1. **Mode mismatch** — install-time probe vs runtime probe gave different
    ///    AP mode results (Concurrent vs Exclusive).
    /// 2. **Channel unusable** — configured channel is DFS/RADAR or disabled in
    ///    the current regulatory domain (e.g. channel 36 in CN).
    /// 3. **Band mismatch** — `hw_mode` in hostapd.conf doesn't match the channel.
    async fn sync_ap_config_with_mode(&self, caps: &NetworkCapabilities) -> NGResult<()> {
        let env_path = format!("{}/{}", AP_CONFIG_DIR, AP_ENV_FILE);
        let current_env = tokio::fs::read_to_string(&env_path)
            .await
            .unwrap_or_default();
        let conf_path = format!("{}/{}", AP_CONFIG_DIR, HOSTAPD_CONF_FILE);
        let current_conf = tokio::fs::read_to_string(&conf_path)
            .await
            .unwrap_or_default();

        // ── 1. Check AP mode (exclusive vs concurrent) ──
        let file_exclusive = parse_env_value(&current_env, "AP_EXCLUSIVE")
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let runtime_exclusive = caps.ap_mode == ApMode::Exclusive;
        let mode_changed = file_exclusive != runtime_exclusive;

        // ── 2. Query kernel for AP-usable channels and validate current config ──
        let file_channel: u32 = parse_conf_value(&current_conf, "channel")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let file_hw_mode = parse_conf_value(&current_conf, "hw_mode").unwrap_or("g".to_string());

        // Get the phy name from the first wireless interface for channel query.
        let phy_name = caps.wireless_interfaces.first().map(|w| w.phy.as_str());

        let usable_channels = if let Some(phy) = phy_name {
            crate::network::capability::detect_ap_usable_channels(phy).await
        } else {
            Vec::new()
        };

        let supported_bands: Vec<WifiBand> = caps
            .wireless_interfaces
            .iter()
            .flat_map(|w| w.supported_bands.iter().cloned())
            .collect();

        // Check if the currently configured channel is usable.
        let current_channel_usable =
            file_channel > 0 && usable_channels.iter().any(|c| c.channel == file_channel);

        // Resolve the effective channel: use current if usable, otherwise pick best.
        let effective_channel = if current_channel_usable {
            file_channel
        } else if !usable_channels.is_empty() {
            let best = ap_config::select_best_ap_channel(&usable_channels)
                .unwrap_or(ap_config::default_channel_for_bands(&supported_bands));
            if file_channel > 0 {
                warn!(
                    old_channel = file_channel,
                    new_channel = best,
                    "Configured AP channel is not usable (DFS/RADAR/disabled) — switching"
                );
            }
            best
        } else {
            ap_config::default_channel_for_bands(&supported_bands)
        };

        let expected_hw_mode = ap_config::hw_mode_for_channel(effective_channel);
        let band_changed =
            file_hw_mode != expected_hw_mode || file_channel == 0 || !current_channel_usable;

        if !mode_changed && !band_changed {
            return Ok(());
        }

        // ── Rewrite files ──
        let sta_iface = parse_env_value(&current_env, "STA_IFACE").unwrap_or("wlan0".to_string());
        let (new_ap_iface, new_exclusive) = if runtime_exclusive {
            (sta_iface.clone(), true)
        } else if mode_changed {
            (format!("{sta_iface}_ap"), false)
        } else {
            (
                parse_env_value(&current_env, "AP_IFACE").unwrap_or(sta_iface.clone()),
                file_exclusive,
            )
        };

        if mode_changed {
            warn!(
                file_exclusive = file_exclusive,
                runtime_exclusive = runtime_exclusive,
                new_ap_iface = %new_ap_iface,
                "AP config/runtime mode mismatch — rewriting"
            );
        }
        if band_changed {
            info!(
                old_hw_mode = %file_hw_mode,
                new_hw_mode = %expected_hw_mode,
                channel = effective_channel,
                "Synchronizing hostapd band configuration with channel"
            );
        }

        // Rewrite ap-env.
        if mode_changed {
            let updated_env = current_env
                .lines()
                .map(|line| {
                    if line.starts_with("AP_IFACE=") {
                        format!("AP_IFACE=\"{new_ap_iface}\"")
                    } else if line.starts_with("AP_EXCLUSIVE=") {
                        format!("AP_EXCLUSIVE=\"{new_exclusive}\"")
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            tokio::fs::write(&env_path, &updated_env)
                .await
                .map_err(|e| NetworkError::ApError(format!("Failed to rewrite ap-env: {e}")))?;
        }

        // Rewrite hostapd.conf — fix interface, hw_mode, channel, and 802.11 flags.
        let is_5ghz = ap_config::is_5ghz_channel(effective_channel);
        let mut conf_lines: Vec<String> = current_conf
            .lines()
            .filter(|line| {
                // Remove old 802.11 and wmm lines — they will be re-added below.
                !line.starts_with("ieee80211n=")
                    && !line.starts_with("ieee80211ac=")
                    && !line.starts_with("wmm_enabled=")
            })
            .map(|line| {
                if mode_changed && line.starts_with("interface=") {
                    format!("interface={new_ap_iface}")
                } else if line.starts_with("hw_mode=") {
                    format!("hw_mode={expected_hw_mode}")
                } else if line.starts_with("channel=") {
                    format!("channel={effective_channel}")
                } else {
                    line.to_string()
                }
            })
            .collect();

        // Insert 802.11 capability lines after the channel= line.
        if let Some(pos) = conf_lines.iter().position(|l| l.starts_with("channel=")) {
            let cap_lines = if is_5ghz {
                vec![
                    "ieee80211n=1".to_string(),
                    "ieee80211ac=1".to_string(),
                    "wmm_enabled=1".to_string(),
                ]
            } else {
                vec!["ieee80211n=1".to_string(), "wmm_enabled=0".to_string()]
            };
            for (i, line) in cap_lines.into_iter().enumerate() {
                conf_lines.insert(pos + 1 + i, line);
            }
        }

        tokio::fs::write(&conf_path, conf_lines.join("\n"))
            .await
            .map_err(|e| NetworkError::ApError(format!("Failed to rewrite hostapd.conf: {e}")))?;

        // Update dnsmasq-ap.conf interface if mode changed.
        if mode_changed {
            let dnsmasq_path = format!("{}/{}", AP_CONFIG_DIR, ap_config::DNSMASQ_AP_CONF_FILE);
            if let Ok(dnsmasq_content) = tokio::fs::read_to_string(&dnsmasq_path).await {
                let updated_dnsmasq = dnsmasq_content
                    .lines()
                    .map(|line| {
                        if line.starts_with("interface=") {
                            format!("interface={new_ap_iface}")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                tokio::fs::write(&dnsmasq_path, &updated_dnsmasq)
                    .await
                    .map_err(|e| {
                        NetworkError::ApError(format!("Failed to rewrite dnsmasq-ap.conf: {e}"))
                    })?;
            }
        }

        info!(
            ap_iface = %new_ap_iface,
            exclusive = new_exclusive,
            hw_mode = %expected_hw_mode,
            channel = effective_channel,
            "AP config files synchronized"
        );
        Ok(())
    }
}

#[async_trait]
impl PlatformNetworkManager for LinuxNetworkManager {
    async fn list_interfaces(&self) -> NGResult<Vec<NetworkInterfaceSummary>> {
        let nm = self.nm_proxy().await?;
        let devices = nm
            .get_devices()
            .await
            .map_err(|e| NetworkError::DBusError(format!("GetDevices failed: {e}")))?;

        // Query all devices concurrently for maximum throughput.
        let futures: Vec<_> = devices
            .iter()
            .map(|dev_path| self.build_interface_summary(dev_path))
            .collect();

        let results = futures::future::join_all(futures).await;

        let mut interfaces = Vec::with_capacity(results.len());
        for result in results {
            match result {
                Ok(Some(summary)) => interfaces.push(summary),
                Ok(None) => {}
                Err(e) => {
                    warn!("Failed to read device: {e}");
                }
            }
        }

        // Sort: Ethernet first, then Wi-Fi, then others.
        interfaces.sort_unstable_by_key(|i| match i.kind {
            InterfaceKind::Ethernet => 0,
            InterfaceKind::Wifi => 1,
            InterfaceKind::Bridge => 2,
            _ => 3,
        });

        Ok(interfaces)
    }

    async fn get_interface(&self, name: &str) -> NGResult<NetworkInterfaceDetail> {
        let nm = self.nm_proxy().await?;

        // Resolve device path directly by interface name (O(1) via NM D-Bus).
        let dev_path = nm
            .get_device_by_ip_iface(name)
            .await
            .map_err(|_| NetworkError::InterfaceNotFound(name.to_string()))?;

        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        let summary = self
            .build_interface_summary(&dev_path)
            .await?
            .ok_or(NetworkError::InterfaceNotFound(name.to_string()))?;

        let dev_props = self
            .get_all_properties(&dev_path_ref, iface::DEVICE)
            .await?;

        let mtu = prop_u32(&dev_props, prop::MTU);
        let driver = prop_str(&dev_props, prop::DRIVER);
        let firmware_version = prop_str(&dev_props, "FirmwareVersion").filter(|v| !v.is_empty());

        // Read NM connection UUID from active connection if present.
        let active_connection_path = prop_object_path(&dev_props, prop::ACTIVE_CONNECTION)
            .filter(|p| !p.is_empty() && p != "/");

        let nm_connection_uuid = if let Some(active_path) = active_connection_path {
            let active_ref = ObjectPath::try_from(active_path.as_str()).ok();
            if let Some(active_ref) = active_ref {
                self.get_all_properties(&active_ref, iface::ACTIVE_CONN)
                    .await
                    .ok()
                    .and_then(|props| prop_str(&props, prop::UUID))
            } else {
                None
            }
        } else {
            None
        };

        let (rx_packets, tx_packets, rx_errors, tx_errors) = read_sysfs_counters(name).await;

        Ok(NetworkInterfaceDetail {
            summary,
            nm_connection_uuid,
            mtu,
            driver,
            firmware_version,
            rx_packets,
            tx_packets,
            rx_errors,
            tx_errors,
        })
    }

    async fn detect_capabilities(&self) -> NGResult<NetworkCapabilities> {
        let nm_version = match self.nm_proxy().await {
            Ok(nm) => nm.version().await.ok(),
            Err(_) => None,
        };

        let nm_available = nm_version.is_some();

        // Discover wireless interfaces and their capabilities.
        let mut wireless_interfaces: Vec<WirelessInterfaceCapability> = Vec::new();

        if nm_available {
            let nm = self.nm_proxy().await?;
            let devices = nm.get_devices().await.unwrap_or_default();
            for dev_path in &devices {
                let dev_path_ref = match ObjectPath::try_from(dev_path.as_str()) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let dev_props = self
                    .get_all_properties(&dev_path_ref, iface::DEVICE)
                    .await
                    .unwrap_or_default();

                if prop_u32(&dev_props, prop::DEVICE_TYPE) != Some(device_type::WIFI) {
                    continue;
                }

                let iface_name = prop_str(&dev_props, prop::INTERFACE).unwrap_or_default();

                if let Some(phy) = resolve_phy_name(&iface_name).await {
                    if let Some(cap) = detect_phy_capabilities(&iface_name, &phy).await {
                        wireless_interfaces.push(cap);
                    }
                }
            }
        }

        let has_wifi = !wireless_interfaces.is_empty();
        let sta_ap = aggregate_sta_ap_capability(&wireless_interfaces);
        let ap_mode = determine_ap_mode(&wireless_interfaces, sta_ap);
        let can_manage_ap = !matches!(ap_mode, ApMode::Unavailable);

        // Cache ap_mode locally so `ap_status` can avoid re-running detect_capabilities.
        *self.cached_ap_mode.write().await = Some(ap_mode);

        Ok(NetworkCapabilities {
            platform: if nm_available {
                PlatformSupport::Full
            } else {
                PlatformSupport::Unavailable
            },
            os: "linux".to_string(),
            arch: env::consts::ARCH.to_string(),
            network_manager_available: nm_available,
            network_manager_version: nm_version,
            can_configure_interfaces: nm_available,
            can_scan_wifi: nm_available && has_wifi,
            can_connect_wifi: nm_available && has_wifi,
            can_manage_ap,
            ap_mode,
            sta_ap_capability: sta_ap,
            wireless_interfaces,
        })
    }

    async fn configure_interface(
        &self,
        name: &str,
        config: &ConfigureInterfaceRequest,
    ) -> NGResult<()> {
        info!(interface = name, method = ?config.ip_config.method(), "Configuring interface");

        let nm = self.nm_proxy().await?;

        let device_path = nm
            .get_device_by_ip_iface(name)
            .await
            .map_err(|_| NetworkError::InterfaceNotFound(name.to_string()))?;

        let dev_path_ref = ObjectPath::try_from(device_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;
        let dev_props = self
            .get_all_properties(&dev_path_ref, iface::DEVICE)
            .await
            .unwrap_or_default();
        let device_type = prop_u32(&dev_props, prop::DEVICE_TYPE).unwrap_or(0);
        let is_wifi = device_type == device_type::WIFI;

        let ip_strings = IpConfigStrings::from_config(&config.ip_config);

        // ── Path A: update an existing connection profile ──
        //
        // For WiFi this is the *only* valid path — a bare WiFi connection without
        // SSID/security is invalid.  For wired interfaces this is the preferred
        // path when a profile already exists.
        if let Some(settings_path) = self.find_connection_for_interface(name).await {
            debug!(interface = name, path = %settings_path, "Updating existing NM connection");

            let settings_ref = ObjectPath::try_from(settings_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid path: {e}")))?;

            // NM Update() replaces ALL settings — GetSettings first, merge our
            // ipv4 changes, then Update.  This preserves WiFi-specific sections
            // (802-11-wireless, 802-11-wireless-security) intact.
            let mut current = self.get_connection_settings(&settings_ref).await.ok_or(
                NetworkError::ConfigError(format!(
                    "Failed to read existing connection settings for {name}"
                )),
            )?;

            let ipv4_owned = build_ipv4_settings_owned(&config.ip_config, &ip_strings);
            current.insert(conn::IPV4.to_string(), ipv4_owned);

            self.dbus_conn
                .call_method(
                    Some(iface::NM),
                    settings_path.as_str(),
                    Some(iface::SETTINGS_CONN),
                    dbus_method::UPDATE,
                    &(current,),
                )
                .await
                .map_err(|e| {
                    NetworkError::ConfigError(format!(
                        "Failed to update connection profile for {name}: {e}"
                    ))
                })?;

            let device_ref = ObjectPath::try_from(device_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid path: {e}")))?;
            let root = ObjectPath::try_from("/")
                .map_err(|e| NetworkError::DBusError(format!("Invalid path: {e}")))?;

            nm.activate_connection(&settings_ref, &device_ref, &root)
                .await
                .map_err(|e| {
                    NetworkError::ConfigError(format!(
                        "Failed to reactivate connection for {name}: {e}"
                    ))
                })?;

            info!(
                interface = name,
                "Interface configuration updated and reactivated"
            );
            return Ok(());
        }

        // ── Path B: WiFi with no saved profile — hard reject ──
        if is_wifi {
            return Err(NetworkError::ConfigError(format!(
                "No existing connection profile found for WiFi interface '{name}'. \
                 Connect to a WiFi network first, then configure its IP settings."
            ))
            .into());
        }

        // ── Path C: wired first-time configuration — create new profile ──
        let conn_id = format!("{name}-config");
        let mut conn_settings: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();

        let mut connection: HashMap<&str, Value<'_>> = HashMap::new();
        connection.insert(conn::ID, Value::from(conn_id.as_str()));
        connection.insert(conn::TYPE, Value::from(conn::ETHERNET));
        connection.insert(conn::INTERFACE_NAME, Value::from(name));
        connection.insert(conn::AUTOCONNECT, Value::from(true));
        conn_settings.insert(conn::CONNECTION, connection);

        let ipv4 = build_nm_ipv4_settings(&config.ip_config, &ip_strings);
        conn_settings.insert(conn::IPV4, ipv4);

        let mut ipv6: HashMap<&str, Value<'_>> = HashMap::new();
        ipv6.insert(conn::METHOD, Value::from(method::AUTO));
        conn_settings.insert(conn::IPV6, ipv6);

        let device_path_ref = ObjectPath::try_from(device_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;
        let root_path = ObjectPath::try_from("/")
            .map_err(|e| NetworkError::DBusError(format!("Invalid root path: {e}")))?;

        nm.add_and_activate_connection(conn_settings, &device_path_ref, &root_path)
            .await
            .map_err(|e| {
                NetworkError::ConfigError(format!("Failed to apply configuration for {name}: {e}"))
            })?;

        info!(
            interface = name,
            "Interface configuration created and applied"
        );
        Ok(())
    }

    async fn scan_wifi(&self, interface_name: Option<&str>) -> NGResult<Vec<WifiAccessPoint>> {
        let dev_path = self.find_wireless_device(interface_name).await?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        // Guard: check device state before attempting scan.
        // When the wireless device is UNAVAILABLE (e.g. hostapd holds the radio in
        // AP mode) or UNMANAGED, NM will reject RequestScan with NotAllowed.
        // Return an empty list instead of propagating a confusing error to the caller.
        let dev_props = self
            .get_all_properties(&dev_path_ref, iface::DEVICE)
            .await
            .unwrap_or_default();
        let dev_state = prop_u32(&dev_props, prop::STATE).unwrap_or(0);

        if dev_state < device_state::DISCONNECTED {
            debug!(
                state = dev_state,
                "Wi-Fi device not ready for scanning (UNAVAILABLE/UNMANAGED), returning empty list"
            );
            return Ok(Vec::new());
        }

        let scan_options: HashMap<&str, Value<'_>> = HashMap::new();
        let wireless_iface = iface::DEVICE_WIRELESS;

        let proxy = self.props_proxy(&dev_path_ref).await?;
        let _: () = proxy
            .inner()
            .connection()
            .call_method(
                Some(iface::NM),
                dev_path.as_str(),
                Some(wireless_iface),
                dbus_method::REQUEST_SCAN,
                &(scan_options,),
            )
            .await
            .map(|m| m.body().deserialize::<()>().unwrap_or(()))
            .map_err(|e| NetworkError::WifiScanFailed(format!("RequestScan failed: {e}")))?;

        // Poll for scan completion with adaptive backoff instead of a fixed 2s sleep.
        // NM updates LastScan timestamp when the scan finishes; we poll until it changes
        // or a timeout is reached.
        let scan_timeout = Duration::from_secs(5);
        let scan_start = Instant::now();
        let mut poll_interval = Duration::from_millis(200);

        let last_scan_before: i64 = self
            .get_property(&dev_path_ref, wireless_iface, prop::LAST_SCAN)
            .await
            .ok()
            .and_then(|v| v.downcast_ref::<i64>().ok())
            .unwrap_or(-1);

        loop {
            sleep(poll_interval).await;

            let last_scan_now: i64 = self
                .get_property(&dev_path_ref, wireless_iface, prop::LAST_SCAN)
                .await
                .ok()
                .and_then(|v| v.downcast_ref::<i64>().ok())
                .unwrap_or(-1);

            if last_scan_now > last_scan_before {
                break;
            }
            if scan_start.elapsed() >= scan_timeout {
                debug!("Wi-Fi scan poll timed out, reading available results");
                break;
            }
            // Exponential backoff: 200ms → 400ms → 800ms (capped)
            poll_interval = (poll_interval * 2).min(Duration::from_millis(800));
        }

        // Read access points.
        let ap_paths: Vec<OwnedObjectPath> = proxy
            .inner()
            .connection()
            .call_method(
                Some(iface::NM),
                dev_path.as_str(),
                Some(wireless_iface),
                dbus_method::GET_ALL_ACCESS_POINTS,
                &(),
            )
            .await
            .map_err(|e| NetworkError::WifiScanFailed(format!("GetAllAccessPoints failed: {e}")))?
            .body()
            .deserialize()
            .map_err(|e| {
                NetworkError::WifiScanFailed(format!("Failed to deserialize AP list: {e}"))
            })?;

        // Check active AP for "is_connected" flag.
        let wifi_props = self
            .get_all_properties(&dev_path_ref, wireless_iface)
            .await
            .unwrap_or_default();
        let active_ap =
            prop_object_path(&wifi_props, prop::ACTIVE_ACCESS_POINT).filter(|p| p.as_str() != "/");

        let mut results = Vec::with_capacity(ap_paths.len());

        for ap_path in &ap_paths {
            let ap_path_ref = match ObjectPath::try_from(ap_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let ap_props = match self
                .get_all_properties(&ap_path_ref, iface::ACCESS_POINT)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    debug!("Failed to read AP {}: {e}", ap_path.as_str());
                    continue;
                }
            };

            let ssid = prop_byte_array(&ap_props, prop::SSID)
                .map(|b| String::from_utf8_lossy(&b).to_string())
                .unwrap_or_default();

            let bssid = prop_str(&ap_props, prop::HW_ADDRESS).unwrap_or_default();
            let strength = prop_u8(&ap_props, prop::STRENGTH).unwrap_or(0);
            let frequency = prop_u32(&ap_props, prop::FREQUENCY).unwrap_or(0);
            let max_bitrate = prop_u32(&ap_props, prop::MAX_BITRATE);
            let flags = prop_u32(&ap_props, prop::FLAGS).unwrap_or(0);
            let wpa_flags = prop_u32(&ap_props, prop::WPA_FLAGS).unwrap_or(0);
            let rsn_flags = prop_u32(&ap_props, prop::RSN_FLAGS).unwrap_or(0);

            let channel = frequency_to_channel(frequency);
            let band = frequency_to_band(frequency);
            let security = derive_security(flags, wpa_flags, rsn_flags);

            let is_connected = active_ap
                .as_ref()
                .is_some_and(|active| active.as_str() == ap_path.as_str());

            results.push(WifiAccessPoint {
                ssid,
                bssid,
                security,
                band,
                channel,
                frequency,
                signal_dbm: quality_to_rssi(strength),
                signal_quality: strength,
                max_bitrate_kbps: max_bitrate,
                is_connected,
            });
        }

        // Remove empty SSIDs, sort by signal descending, then deduplicate by SSID.
        // Since the list is sorted by signal strength, `dedup_by` keeps the strongest.
        results.retain(|ap| !ap.ssid.is_empty());
        results.sort_unstable_by(|a, b| {
            a.ssid
                .cmp(&b.ssid)
                .then(b.signal_quality.cmp(&a.signal_quality))
        });
        results.dedup_by(|a, b| a.ssid == b.ssid);
        results.sort_unstable_by(|a, b| b.signal_quality.cmp(&a.signal_quality));

        Ok(results)
    }

    async fn wifi_connect_preflight(
        &self,
        request: &WifiConnectRequest,
    ) -> NGResult<WifiConnectPreflight> {
        let caps = self.detect_capabilities().await?;
        let ap_status = self.ap_status().await.unwrap_or(ApStatus {
            active: false,
            interface_name: None,
            ssid: None,
            band: None,
            channel: None,
            frequency: None,
            security: None,
            connected_clients: None,
            ip_address: None,
            prefix_length: None,
            ap_mode: caps.ap_mode,
            sta_will_disconnect: false,
            sta_restore_failed: false,
        });

        let ap_running = ap_status.active;
        let is_exclusive = caps.ap_mode == ApMode::Exclusive;

        let ap_will_stop = ap_running && is_exclusive;
        let connection_will_be_lost = ap_will_stop;

        // Probe whether we can restore AP on a virtual interface after STA connects.
        let ap_can_restore = if ap_will_stop {
            caps.wireless_interfaces
                .iter()
                .any(|i| i.supports_sta_ap_concurrent)
        } else {
            false
        };

        let mut warnings = Vec::new();

        if ap_will_stop {
            if ap_can_restore {
                warnings.push(
                    "AP hotspot will briefly stop during the switch. \
                     It will be automatically restored on a virtual interface after \
                     Wi-Fi connection succeeds."
                        .to_string(),
                );
            } else {
                warnings.push(
                    "AP hotspot will be stopped to free the wireless interface. \
                     You will lose your current connection to the gateway. \
                     After connecting, access the gateway via the new Wi-Fi \
                     network's IP address."
                        .to_string(),
                );
            }
        }

        Ok(WifiConnectPreflight {
            ssid: request.ssid.clone(),
            ap_will_stop,
            connection_will_be_lost,
            ap_can_restore,
            warnings,
        })
    }

    async fn connect_wifi(&self, request: &WifiConnectRequest) -> NGResult<WifiStaStatus> {
        info!(ssid = %request.ssid, "Connecting to Wi-Fi network");

        // ─── Orchestrate AP stop if needed (exclusive mode) ───
        let preflight = self.wifi_connect_preflight(request).await?;
        if preflight.ap_will_stop {
            info!("AP is running in exclusive mode — stopping AP before STA connect");
            let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
            mgr.stop_all().await?;
            sleep(Duration::from_millis(500)).await;
        }

        let dev_path = self
            .find_wireless_device(request.interface_name.as_deref())
            .await?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        let nm = self.nm_proxy().await?;

        // Build NM connection settings for 802-11-wireless.
        let mut conn_settings: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();

        let mut connection: HashMap<&str, Value<'_>> = HashMap::new();
        connection.insert(conn::ID, Value::from(request.ssid.as_str()));
        connection.insert(conn::TYPE, Value::from(conn::WIFI));
        connection.insert(conn::AUTOCONNECT, Value::from(true));
        conn_settings.insert(conn::CONNECTION, connection);

        let mut wireless: HashMap<&str, Value<'_>> = HashMap::new();
        let ssid_bytes: Vec<u8> = request.ssid.as_bytes().to_vec();
        wireless.insert(conn::WIFI_SSID, Value::from(ssid_bytes));
        wireless.insert(conn::WIFI_MODE, Value::from(wifi_mode::INFRASTRUCTURE));
        if request.hidden.unwrap_or(false) {
            wireless.insert(conn::WIFI_HIDDEN, Value::from(true));
        }
        if let Some(bssid) = &request.bssid {
            wireless.insert(conn::WIFI_BSSID, Value::from(bssid.as_str()));
        }
        conn_settings.insert(conn::WIFI, wireless);

        if let Some(password) = &request.password {
            if !password.is_empty() {
                let mut security: HashMap<&str, Value<'_>> = HashMap::new();
                security.insert(conn::KEY_MGMT, Value::from(key_mgmt::WPA_PSK));
                security.insert(conn::PSK, Value::from(password.as_str()));
                conn_settings.insert(conn::WIFI_SECURITY, security);
            }
        }

        let effective_ip_config = request
            .ip_config
            .as_ref()
            .cloned()
            .unwrap_or(IpConfig::Dhcp {
                dns: None,
                search_domains: None,
            });
        let ip_strings = IpConfigStrings::from_config(&effective_ip_config);
        let ipv4 = build_nm_ipv4_settings(&effective_ip_config, &ip_strings);
        conn_settings.insert(conn::IPV4, ipv4);

        let mut ipv6: HashMap<&str, Value<'_>> = HashMap::new();
        ipv6.insert(conn::METHOD, Value::from(method::AUTO));
        conn_settings.insert(conn::IPV6, ipv6);

        let root_path = ObjectPath::try_from("/")
            .map_err(|e| NetworkError::DBusError(format!("Invalid root path: {e}")))?;

        let (conn_settings_path, active_conn_path) = nm
            .add_and_activate_connection(conn_settings, &dev_path_ref, &root_path)
            .await
            .map_err(|e| {
                NetworkError::WifiError(format!("Failed to connect to '{}': {e}", request.ssid))
            })?;

        // Poll connection state until activated or failed.
        let timeout_secs: u64 = 30;
        let start = Instant::now();
        let active_path_ref = ObjectPath::try_from(active_conn_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid active conn path: {e}")))?;

        let result = loop {
            if start.elapsed() > Duration::from_secs(timeout_secs) {
                break Err(NetworkError::WifiConnectionTimeout {
                    ssid: request.ssid.clone(),
                    timeout_secs,
                });
            }

            let state = self
                .get_property(&active_path_ref, iface::ACTIVE_CONN, prop::STATE)
                .await
                .ok()
                .and_then(|v| v.downcast_ref::<u32>().ok())
                .unwrap_or(0);

            match state {
                active_conn_state::ACTIVATED => {
                    info!(ssid = %request.ssid, "Wi-Fi connection established");
                    break Ok(());
                }
                active_conn_state::DEACTIVATED => {
                    break Err(NetworkError::WifiError(format!(
                        "Connection to '{}' was rejected or failed",
                        request.ssid
                    )));
                }
                _ => {
                    sleep(Duration::from_millis(500)).await;
                }
            }
        };

        // On failure or timeout, clean up the NM connection profile and — critically
        // — if we stopped AP to attempt the connection, restart it so the user can
        // still reach the gateway via the AP hotspot.
        if let Err(e) = result {
            warn!(error = %e, "Wi-Fi connection failed, cleaning up NM connection profile");
            self.cleanup_connection_profile(&active_conn_path, &conn_settings_path)
                .await;

            if preflight.ap_will_stop {
                warn!("STA connection failed after AP was stopped — restarting AP as fallback");
                let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
                if let Err(ap_err) = mgr.start_all().await {
                    warn!(error = %ap_err, "Failed to restart AP fallback after STA failure");
                } else {
                    info!("AP restarted as fallback — gateway is reachable via AP hotspot");
                }
            }

            return Err(e.into());
        }

        // ─── Post-connect AP handling ───
        if preflight.ap_will_stop {
            if preflight.ap_can_restore {
                // Hardware supports concurrent STA+AP (e.g. Realtek via virtual interface).
                info!("STA connected — restoring AP on virtual interface");
                if let Err(e) = self.try_restore_ap_concurrent().await {
                    warn!(error = %e, "Failed to restore AP in concurrent mode after STA connect");
                }
            } else {
                // True exclusive mode — AP stays off, STA is now the management channel.
                info!(
                    "STA connected in exclusive mode — AP remains off. \
                     Gateway is now reachable via the new Wi-Fi network."
                );
            }
        }

        self.wifi_sta_status(request.interface_name.as_deref())
            .await
    }

    async fn disconnect_wifi(&self, request: &WifiDisconnectRequest) -> NGResult<()> {
        info!("Disconnecting Wi-Fi STA");

        self.disconnect_wifi_inner(
            request.interface_name.as_deref(),
            request.disable_autoconnect,
        )
        .await?;

        // Evaluate AP restore only for user-initiated disconnects (via API).
        // Internal callers (e.g. start_ap) manage AP lifecycle themselves.
        self.evaluate_and_restore_ap().await;

        Ok(())
    }

    async fn list_saved_wifi_connections(&self) -> NGResult<Vec<SavedWifiConnection>> {
        let conn_paths: Vec<OwnedObjectPath> = self
            .dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path::ROOT,
                Some(iface::SETTINGS),
                dbus_method::LIST_CONNECTIONS,
                &(),
            )
            .await
            .map_err(|e| NetworkError::DBusError(format!("ListConnections failed: {e}")))?
            .body()
            .deserialize::<Vec<OwnedObjectPath>>()
            .map_err(|e| NetworkError::DBusError(format!("ListConnections parse failed: {e}")))?;

        // Collect active connection UUIDs for is_active determination.
        let active_uuids = self.collect_active_connection_uuids().await;

        let mut saved: Vec<SavedWifiConnection> = Vec::new();

        for conn_path in &conn_paths {
            let conn_path_ref = match ObjectPath::try_from(conn_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let settings = match self.get_connection_settings(&conn_path_ref).await {
                Some(s) => s,
                None => continue,
            };

            let conn_section = match settings.get(conn::CONNECTION) {
                Some(s) => s,
                None => continue,
            };

            let conn_type = settings_str(conn_section, conn::TYPE);
            if conn_type.as_deref() != Some(conn::WIFI) {
                continue;
            }

            let uuid = match settings_str(conn_section, conn::UUID) {
                Some(u) => u,
                None => continue,
            };

            let ssid = settings
                .get(conn::WIFI)
                .and_then(|wifi_s| {
                    wifi_s.get(conn::WIFI_SSID).and_then(|v| {
                        v.downcast_ref::<&zbus::zvariant::Array>().ok().map(|arr| {
                            let bytes: Vec<u8> = arr
                                .iter()
                                .filter_map(|i| i.downcast_ref::<u8>().ok())
                                .collect();
                            String::from_utf8_lossy(&bytes).to_string()
                        })
                    })
                })
                .unwrap_or_default();

            if ssid.is_empty() {
                continue;
            }

            let autoconnect = settings_bool(conn_section, conn::AUTOCONNECT).unwrap_or(true);
            let timestamp = settings_u64(conn_section, conn::TIMESTAMP);

            let security = settings
                .get(conn::WIFI_SECURITY)
                .and_then(|sec_s| settings_str(sec_s, conn::KEY_MGMT))
                .map(|km| match km.as_str() {
                    "wpa-psk" => WifiSecurity::Wpa2Psk,
                    "sae" => WifiSecurity::Wpa3Sae,
                    "wpa-eap" | "wpa-eap-suite-b-192" => WifiSecurity::Wpa2Enterprise,
                    "ieee8021x" => WifiSecurity::WpaEnterprise,
                    "none" => WifiSecurity::Open,
                    _ => WifiSecurity::Unknown,
                })
                .unwrap_or(WifiSecurity::Open);

            let ip_config = settings
                .get(conn::IPV4)
                .and_then(|ipv4_s| settings_str(ipv4_s, conn::METHOD))
                .map(|m| match m.as_str() {
                    method::MANUAL => {
                        let (ip_address, prefix_length) = settings
                            .get(conn::IPV4)
                            .and_then(parse_settings_address_data)
                            .unwrap_or((
                                "0.0.0.0"
                                    .parse()
                                    .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                                24,
                            ));
                        let gateway = settings
                            .get(conn::IPV4)
                            .and_then(|ipv4_s| settings_str(ipv4_s, conn::GATEWAY))
                            .and_then(|g| g.parse::<IpAddr>().ok());
                        let dns = settings
                            .get(conn::IPV4)
                            .map(parse_settings_dns)
                            .filter(|d| !d.is_empty());
                        let search_domains = settings
                            .get(conn::IPV4)
                            .map(parse_settings_search_domains)
                            .filter(|d| !d.is_empty());
                        IpConfig::Static {
                            config: StaticIpConfig {
                                ip_address,
                                prefix_length,
                                gateway,
                                dns,
                                search_domains,
                            },
                        }
                    }
                    method::DISABLED => IpConfig::Disabled,
                    _ => IpConfig::Dhcp {
                        dns: None,
                        search_domains: None,
                    },
                })
                .unwrap_or(IpConfig::Dhcp {
                    dns: None,
                    search_domains: None,
                });

            let is_active = active_uuids.contains(&uuid);

            saved.push(SavedWifiConnection {
                uuid,
                ssid,
                is_active,
                autoconnect,
                security,
                ip_config,
                last_connected: timestamp,
            });
        }

        // Sort: active first, then by last_connected descending.
        saved.sort_by(|a, b| {
            b.is_active
                .cmp(&a.is_active)
                .then(b.last_connected.cmp(&a.last_connected))
        });

        Ok(saved)
    }

    async fn forget_wifi(&self, request: &ForgetWifiRequest) -> NGResult<()> {
        info!(uuid = %request.uuid, "Forgetting saved Wi-Fi connection");

        let conn_paths: Vec<OwnedObjectPath> = self
            .dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path::ROOT,
                Some(iface::SETTINGS),
                dbus_method::LIST_CONNECTIONS,
                &(),
            )
            .await
            .map_err(|e| NetworkError::DBusError(format!("ListConnections failed: {e}")))?
            .body()
            .deserialize::<Vec<OwnedObjectPath>>()
            .map_err(|e| NetworkError::DBusError(format!("ListConnections parse failed: {e}")))?;

        let mut target_path: Option<String> = None;
        let mut target_ssid = String::new();

        for conn_path in &conn_paths {
            let conn_path_ref = match ObjectPath::try_from(conn_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let settings = match self.get_connection_settings(&conn_path_ref).await {
                Some(s) => s,
                None => continue,
            };

            let conn_section = match settings.get(conn::CONNECTION) {
                Some(s) => s,
                None => continue,
            };

            let conn_type = settings_str(conn_section, conn::TYPE);
            if conn_type.as_deref() != Some(conn::WIFI) {
                continue;
            }

            let uuid = settings_str(conn_section, conn::UUID);

            if uuid.as_deref() == Some(&request.uuid) {
                target_path = Some(conn_path.to_string());
                target_ssid = settings_str(conn_section, conn::ID).unwrap_or(request.uuid.clone());
                break;
            }
        }

        let settings_path =
            target_path.ok_or(NetworkError::WifiConnectionNotFound(request.uuid.clone()))?;

        // If the connection is currently active, deactivate it first.
        let nm = self.nm_proxy().await?;
        if let Some(active_path) = self.find_active_connection_by_uuid(&request.uuid).await {
            let active_ref = ObjectPath::try_from(active_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid path: {e}")))?;
            if let Err(e) = nm.deactivate_connection(&active_ref).await {
                return Err(NetworkError::WifiForgetFailed {
                    ssid: target_ssid,
                    reason: format!("Deactivation failed: {e}"),
                }
                .into());
            }
            info!(ssid = %target_ssid, "Deactivated active connection before deletion");
        }

        // Delete the connection profile.
        self.dbus_conn
            .call_method(
                Some(iface::NM),
                settings_path.as_str(),
                Some(iface::SETTINGS_CONN),
                dbus_method::DELETE,
                &(),
            )
            .await
            .map_err(|e| NetworkError::WifiForgetFailed {
                ssid: target_ssid.clone(),
                reason: format!("Delete failed: {e}"),
            })?;

        info!(ssid = %target_ssid, uuid = %request.uuid, "Wi-Fi connection profile deleted");

        self.evaluate_and_restore_ap().await;

        Ok(())
    }

    async fn wifi_sta_status(&self, interface_name: Option<&str>) -> NGResult<WifiStaStatus> {
        let dev_path = match self.find_wireless_device(interface_name).await {
            Ok(p) => p,
            Err(_) => {
                return Ok(WifiStaStatus::default());
            }
        };

        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        let dev_props = self
            .get_all_properties(&dev_path_ref, iface::DEVICE)
            .await?;

        let iface_name = prop_str(&dev_props, prop::INTERFACE);
        let state = prop_u32(&dev_props, prop::STATE).unwrap_or(0);
        let speed = prop_u32(&dev_props, prop::SPEED);

        if state != device_state::ACTIVATED {
            return Ok(WifiStaStatus {
                interface_name: iface_name,
                speed_mbps: speed,
                ..Default::default()
            });
        }

        // Read active AP properties.
        let wifi_props = self
            .get_all_properties(&dev_path_ref, iface::DEVICE_WIRELESS)
            .await
            .unwrap_or_default();

        let (ssid, bssid, security, band, channel, frequency, signal_dbm, signal_quality) =
            if let Some(ap_path) =
                prop_object_path(&wifi_props, prop::ACTIVE_ACCESS_POINT).filter(|p| p != "/")
            {
                let ap_path_ref = ObjectPath::try_from(ap_path.as_str())
                    .map_err(|e| NetworkError::DBusError(format!("Invalid AP path: {e}")))?;
                let ap_props = self
                    .get_all_properties(&ap_path_ref, iface::ACCESS_POINT)
                    .await
                    .unwrap_or_default();

                let ssid = prop_byte_array(&ap_props, prop::SSID)
                    .map(|b| String::from_utf8_lossy(&b).to_string());
                let bssid = prop_str(&ap_props, prop::HW_ADDRESS);
                let strength = prop_u8(&ap_props, prop::STRENGTH).unwrap_or(0);
                let freq = prop_u32(&ap_props, prop::FREQUENCY).unwrap_or(0);
                let flags = prop_u32(&ap_props, prop::FLAGS).unwrap_or(0);
                let wpa_flags = prop_u32(&ap_props, prop::WPA_FLAGS).unwrap_or(0);
                let rsn_flags = prop_u32(&ap_props, prop::RSN_FLAGS).unwrap_or(0);

                (
                    ssid,
                    bssid,
                    Some(derive_security(flags, wpa_flags, rsn_flags)),
                    Some(frequency_to_band(freq)),
                    Some(frequency_to_channel(freq)),
                    Some(freq),
                    Some(quality_to_rssi(strength)),
                    Some(strength),
                )
            } else {
                (None, None, None, None, None, None, None, None)
            };

        // Read IP configuration.
        let ip4 = self.read_ipv4_config(&dev_props).await.ok();
        let ip_address = ip4
            .as_ref()
            .and_then(|c| c.addresses.first())
            .map(|a| a.address);
        let gateway = ip4.as_ref().and_then(|c| c.gateway);
        let dns = ip4.as_ref().map(|c| c.dns.clone()).unwrap_or_default();

        Ok(WifiStaStatus {
            connected: true,
            interface_name: iface_name,
            ssid,
            bssid,
            security,
            band,
            channel,
            frequency,
            signal_dbm,
            signal_quality,
            ip_address,
            gateway,
            dns,
            speed_mbps: speed,
            connected_secs: None,
        })
    }

    async fn ap_status(&self) -> NGResult<ApStatus> {
        // Use locally cached ap_mode to avoid expensive detect_capabilities on every call.
        // Falls back to a fresh detection if no cached value exists.
        let ap_mode = {
            let cached = self.cached_ap_mode.read().await;
            match *cached {
                Some(mode) => mode,
                None => {
                    drop(cached);
                    let caps = self.detect_capabilities().await.ok();
                    caps.map(|c| c.ap_mode).unwrap_or(ApMode::Unavailable)
                }
            }
        };
        let sta_will_disconnect = ap_mode == ApMode::Exclusive;
        let sta_restore_failed = *self.sta_restore_failed.read().await;

        let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
        let svc_status = mgr.status().await.unwrap_or(ApServiceStatus {
            setup_active: false,
            hostapd_active: false,
            dnsmasq_active: false,
        });

        if !svc_status.ap_broadcasting() {
            return Ok(ApStatus {
                active: false,
                interface_name: None,
                ssid: None,
                band: None,
                channel: None,
                frequency: None,
                security: None,
                connected_clients: None,
                ip_address: None,
                prefix_length: None,
                ap_mode,
                sta_will_disconnect,
                sta_restore_failed,
            });
        }

        // Read hostapd.conf for SSID / channel / hw_mode.
        let conf_path = format!("{AP_CONFIG_DIR}/{HOSTAPD_CONF_FILE}");
        let conf_content = tokio::fs::read_to_string(&conf_path)
            .await
            .unwrap_or_default();

        let ssid = parse_conf_value(&conf_content, "ssid");
        let channel: Option<u32> =
            parse_conf_value(&conf_content, "channel").and_then(|s| s.parse().ok());
        let iface = parse_conf_value(&conf_content, "interface");

        let band = match parse_conf_value(&conf_content, "hw_mode").as_deref() {
            Some("a") => Some(WifiBand::Band5Ghz),
            Some("g") | Some("b") => Some(WifiBand::Band2_4Ghz),
            _ => Some(WifiBand::Band2_4Ghz),
        };

        let frequency = channel.map(|ch| match ch {
            14 => 2484,
            1..=13 => 2407 + ch * 5,
            _ => 5000 + ch * 5,
        });

        // Read ap-env for IP info.
        let env_path = format!("{AP_CONFIG_DIR}/{AP_ENV_FILE}");
        let env_content = tokio::fs::read_to_string(&env_path)
            .await
            .unwrap_or_default();

        let ip_str = parse_env_value(&env_content, "AP_IP");
        let prefix_str = parse_env_value(&env_content, "AP_PREFIX");
        let ip_address = ip_str.and_then(|s| s.parse::<IpAddr>().ok());
        let prefix_length: Option<u8> = prefix_str.and_then(|s| s.parse().ok());

        let connected_clients = count_hostapd_clients(&iface.clone().unwrap_or_default()).await;

        Ok(ApStatus {
            active: true,
            interface_name: iface,
            ssid,
            band,
            channel,
            frequency,
            security: Some(parse_hostapd_security(&conf_content)),
            connected_clients,
            ip_address,
            prefix_length,
            ap_mode,
            sta_will_disconnect,
            sta_restore_failed,
        })
    }

    async fn start_ap(&self) -> NGResult<ApStatus> {
        info!("Starting AP hotspot...");

        let caps = self.detect_capabilities().await?;
        if !caps.can_manage_ap {
            return Err(NetworkError::ApError(
                "AP management is not available on this hardware".into(),
            )
            .into());
        }

        self.sync_ap_config_with_mode(&caps).await?;

        let is_exclusive = caps.ap_mode == ApMode::Exclusive;

        // In exclusive mode, stash the current STA connection info so we can
        // restore it when the user stops AP later. We do NOT disconnect STA
        // here — ap-setup.sh handles the full interface lifecycle:
        //   release from NM → down → set type __ap → up → assign IP
        // Doing a D-Bus DeactivateConnection here would race with the shell
        // script's nmcli operations and leave the interface in an inconsistent
        // state.
        if is_exclusive {
            let mut restore_failed = self.sta_restore_failed.write().await;
            *restore_failed = false;
            drop(restore_failed);

            info!("Exclusive mode: stashing STA connection for later restore");
            if let Some(stashed) = self.stash_active_sta_connection().await {
                stashed.persist().await;
                let mut guard = self.stashed_sta_for_restore.write().await;
                *guard = Some(stashed);
            }
        }

        let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
        match mgr.start_all().await {
            Ok(()) => {
                info!("AP hotspot started");
                self.ap_status().await
            }
            Err(e) => {
                tracing::error!(error = %e, "AP service stack failed to start");
                if is_exclusive {
                    warn!("Rolling back: stopping failed AP services and restoring STA");
                    if let Err(stop_err) = mgr.stop_all().await {
                        warn!(error = %stop_err, "Failed to stop partially-started AP services");
                    }
                    // Force interface back to managed mode via the teardown script,
                    // then restore the previous STA connection.
                    self.force_restore_wifi_interface().await;
                    self.restore_stashed_sta_connection().await;
                }
                Err(e)
            }
        }
    }

    async fn stop_ap(&self) -> NGResult<ApStatus> {
        info!("Stopping AP hotspot...");

        let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
        mgr.stop_all().await?;

        // In exclusive mode, restore the previously disconnected STA connection
        // and clean up the persisted restore file.
        //
        // ap-teardown.sh (ExecStop) handles low-level restoration: switching the
        // interface back to managed type and handing it to NM. We wait briefly
        // for NM to re-discover the interface, then activate the stashed connection.
        self.restore_stashed_sta_connection().await;

        info!("AP hotspot stopped");
        self.ap_status().await
    }

    async fn configure_ap(&self, config: &ConfigureApRequest) -> NGResult<ApStatus> {
        info!(ssid = ?config.ssid, channel = ?config.channel, "Configuring AP hotspot");

        // Detect hardware capabilities for supported_bands.
        let caps = self.detect_capabilities().await?;
        let supported_bands: Vec<WifiBand> = caps
            .wireless_interfaces
            .iter()
            .flat_map(|w| w.supported_bands.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        // Read current config for merge.
        let conf_path = format!("{}/{}", AP_CONFIG_DIR, ap_config::HOSTAPD_CONF_FILE);
        let current_conf = tokio::fs::read_to_string(&conf_path)
            .await
            .unwrap_or_default();
        let env_path = format!("{}/{}", AP_CONFIG_DIR, ap_config::AP_ENV_FILE);
        let current_env = tokio::fs::read_to_string(&env_path)
            .await
            .unwrap_or_default();

        let current_ssid = parse_conf_value(&current_conf, "ssid").unwrap_or_default();
        let current_password =
            parse_conf_value(&current_conf, "wpa_passphrase").unwrap_or_default();
        let current_channel: u32 = parse_conf_value(&current_conf, "channel")
            .and_then(|s| s.parse().ok())
            .unwrap_or(6);
        let current_country = parse_conf_value(&current_conf, "country_code")
            .unwrap_or(ap_config::DEFAULT_COUNTRY_CODE.to_string());
        let current_iface =
            parse_env_value(&current_env, "AP_IFACE").unwrap_or("wlan0_ap".to_string());
        let current_ip = parse_env_value(&current_env, "AP_IP").unwrap_or("10.47.0.1".to_string());
        let current_prefix: u8 = parse_env_value(&current_env, "AP_PREFIX")
            .and_then(|s| s.parse().ok())
            .unwrap_or(24);
        let current_dhcp_start =
            parse_env_value(&current_env, "AP_DHCP_START").unwrap_or("10.47.0.10".to_string());
        let current_dhcp_end =
            parse_env_value(&current_env, "AP_DHCP_END").unwrap_or("10.47.0.200".to_string());
        let current_sta_iface =
            parse_env_value(&current_env, "STA_IFACE").unwrap_or("wlan0".to_string());
        let current_uplink_iface =
            parse_env_value(&current_env, "UPLINK_IFACE").unwrap_or(current_sta_iface.clone());
        let current_exclusive = parse_env_value(&current_env, "AP_EXCLUSIVE")
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let ctx = ApRenderContext {
            interface: current_iface,
            ssid: config.ssid.clone().unwrap_or(current_ssid),
            password: config.password.clone().unwrap_or(current_password),
            channel: config.channel.unwrap_or(current_channel),
            ip: current_ip,
            prefix_length: current_prefix,
            dhcp_range_start: current_dhcp_start,
            dhcp_range_end: current_dhcp_end,
            dhcp_lease_time: "12h".to_string(),
            sta_iface: current_sta_iface,
            uplink_iface: current_uplink_iface,
            exclusive: current_exclusive,
            country_code: config.country_code.clone().unwrap_or(current_country),
            supported_bands,
        };

        // Backup → render → rollback on failure.
        ap_config::backup_ap_config(AP_CONFIG_DIR).await?;

        if let Err(e) = ap_config::render_and_write_ap_config(&ctx, AP_CONFIG_DIR).await {
            warn!(error = %e, "Failed to render AP config, restoring backup");
            ap_config::restore_ap_config(AP_CONFIG_DIR).await?;
            return Err(e);
        }

        // Only restart hostapd if AP is currently running.
        let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
        let svc_status = mgr.status().await.ok();
        let ap_running = svc_status
            .as_ref()
            .map(|s| s.ap_broadcasting())
            .unwrap_or(false);

        let should_restart = config.restart.unwrap_or(true) && ap_running;
        if should_restart {
            if let Err(e) = mgr.restart_hostapd().await {
                warn!(error = %e, "hostapd restart failed, restoring backup");
                ap_config::restore_ap_config(AP_CONFIG_DIR).await?;
                if let Err(restore_err) = mgr.restart_hostapd().await {
                    tracing::error!(
                        error = %restore_err,
                        "CRITICAL: hostapd restart after rollback also failed — AP is in degraded state"
                    );
                }
                return Err(NetworkError::ApConfigRollback {
                    reason: e.to_string(),
                }
                .into());
            }
        } else if !ap_running {
            info!("AP is not running — configuration saved, will take effect on next start");
        }

        self.ap_status().await
    }
}

// ─── Helper Functions ───

// ─── Shared IPv4 NM Settings Builder ───

/// Pre-computed owned string representations of [`IpConfig`] fields.
///
/// Because NM D-Bus expects `Value<'a>` borrowing from string data, we need
/// owned strings that outlive the `HashMap<&str, Value<'_>>`. This struct
/// holds those strings so both `configure_interface` and `connect_wifi` can
/// borrow from a single allocation.
struct IpConfigStrings {
    addr_str: Option<String>,
    gw_str: Option<String>,
}

impl IpConfigStrings {
    fn from_config(config: &IpConfig) -> Self {
        match config {
            IpConfig::Static { config: sc } => Self {
                addr_str: Some(sc.ip_address.to_string()),
                gw_str: sc.gateway.as_ref().map(|g| g.to_string()),
            },
            _ => Self {
                addr_str: None,
                gw_str: None,
            },
        }
    }
}

/// Infallible `Value<'_>` → `OwnedValue` conversion.
///
/// `OwnedValue::try_from(Value)` only fails for fd-passing variants which we
/// never construct. This helper avoids scattering `.unwrap()` across the
/// settings builders while keeping the code honest about the invariant.
#[inline]
fn to_owned_value(v: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(v).expect("BUG: Value→OwnedValue conversion failed for non-fd variant")
}

/// Build NM IPv4 settings as `HashMap<String, OwnedValue>` for merging into
/// existing connection settings via GetSettings → modify → Update.
///
/// Unlike [`build_nm_ipv4_settings`] (which returns borrowed `Value<'a>`), this
/// produces owned values compatible with the `HashMap<String, HashMap<String, OwnedValue>>`
/// returned by `GetSettings()`.
fn build_ipv4_settings_owned(
    ip_config: &IpConfig,
    strings: &IpConfigStrings,
) -> HashMap<String, OwnedValue> {
    let mut ipv4: HashMap<String, OwnedValue> = HashMap::new();
    match ip_config {
        IpConfig::Dhcp {
            dns,
            search_domains,
        } => {
            ipv4.insert(
                conn::METHOD.to_string(),
                to_owned_value(Value::from(method::AUTO)),
            );
            if let Some(dns_addrs) = dns {
                let dns_u32s = ip_addrs_to_nm_dns(dns_addrs);
                if !dns_u32s.is_empty() {
                    ipv4.insert(conn::DNS.to_string(), to_owned_value(Value::from(dns_u32s)));
                }
            }
            if let Some(domains) = search_domains {
                if !domains.is_empty() {
                    let domain_strs: Vec<&str> = domains.iter().map(|s| s.as_str()).collect();
                    ipv4.insert(
                        conn::DNS_SEARCH.to_string(),
                        to_owned_value(Value::from(domain_strs)),
                    );
                }
            }
        }
        IpConfig::Static { config: sc } => {
            ipv4.insert(
                conn::METHOD.to_string(),
                to_owned_value(Value::from(method::MANUAL)),
            );

            if let Some(ip_s) = strings.addr_str.as_deref() {
                let prefix_u32 = sc.prefix_length as u32;
                let mut addr_dict: HashMap<&str, Value<'_>> = HashMap::new();
                addr_dict.insert(prop::ADDR_KEY_ADDRESS, Value::from(ip_s));
                addr_dict.insert(prop::ADDR_KEY_PREFIX, Value::from(prefix_u32));
                if let Ok(v) = OwnedValue::try_from(Value::from(vec![addr_dict])) {
                    ipv4.insert(conn::ADDRESS_DATA.to_string(), v);
                }
            }

            if let Some(gw) = strings.gw_str.as_deref() {
                ipv4.insert(conn::GATEWAY.to_string(), to_owned_value(Value::from(gw)));
            }

            let dns_u32s: Vec<u32> = ip_addrs_to_nm_dns(sc.dns.as_deref().unwrap_or_default());
            if !dns_u32s.is_empty() {
                ipv4.insert(conn::DNS.to_string(), to_owned_value(Value::from(dns_u32s)));
            }

            if let Some(domains) = &sc.search_domains {
                if !domains.is_empty() {
                    let domain_strs: Vec<&str> = domains.iter().map(|s| s.as_str()).collect();
                    ipv4.insert(
                        conn::DNS_SEARCH.to_string(),
                        to_owned_value(Value::from(domain_strs)),
                    );
                }
            }
        }
        IpConfig::Disabled => {
            ipv4.insert(
                conn::METHOD.to_string(),
                to_owned_value(Value::from(method::DISABLED)),
            );
        }
    }
    ipv4
}

/// Build NM D-Bus IPv4 settings dict from the unified [`IpConfig`] enum.
///
/// Shared between `configure_interface` (path C) and `connect_wifi` to
/// eliminate duplicated IP configuration logic. The `strings` parameter must
/// be pre-computed via [`IpConfigStrings::from_config`] and must outlive the
/// returned `HashMap`.
fn build_nm_ipv4_settings<'a>(
    ip_config: &'a IpConfig,
    strings: &'a IpConfigStrings,
) -> HashMap<&'a str, Value<'a>> {
    let mut ipv4: HashMap<&str, Value<'_>> = HashMap::new();
    match ip_config {
        IpConfig::Dhcp {
            dns,
            search_domains,
        } => {
            ipv4.insert(conn::METHOD, Value::from(method::AUTO));
            if let Some(dns_addrs) = dns {
                let dns_u32s = ip_addrs_to_nm_dns(dns_addrs);
                if !dns_u32s.is_empty() {
                    ipv4.insert(conn::DNS, Value::from(dns_u32s));
                }
            }
            if let Some(domains) = search_domains {
                if !domains.is_empty() {
                    let domain_strs: Vec<&str> = domains.iter().map(|s| s.as_str()).collect();
                    ipv4.insert(conn::DNS_SEARCH, Value::from(domain_strs));
                }
            }
        }
        IpConfig::Static { config: sc } => {
            ipv4.insert(conn::METHOD, Value::from(method::MANUAL));

            if let Some(ip_s) = strings.addr_str.as_deref() {
                let prefix_u32 = sc.prefix_length as u32;
                let mut addr_dict: HashMap<&str, Value<'_>> = HashMap::new();
                addr_dict.insert(prop::ADDR_KEY_ADDRESS, Value::from(ip_s));
                addr_dict.insert(prop::ADDR_KEY_PREFIX, Value::from(prefix_u32));
                ipv4.insert(conn::ADDRESS_DATA, Value::from(vec![addr_dict]));
            }

            if let Some(gw) = strings.gw_str.as_deref() {
                ipv4.insert(conn::GATEWAY, Value::from(gw));
            }

            let dns_u32s: Vec<u32> = ip_addrs_to_nm_dns(sc.dns.as_deref().unwrap_or_default());
            if !dns_u32s.is_empty() {
                ipv4.insert(conn::DNS, Value::from(dns_u32s));
            }

            if let Some(domains) = &sc.search_domains {
                if !domains.is_empty() {
                    let domain_strs: Vec<&str> = domains.iter().map(|s| s.as_str()).collect();
                    ipv4.insert(conn::DNS_SEARCH, Value::from(domain_strs));
                }
            }
        }
        IpConfig::Disabled => {
            ipv4.insert(conn::METHOD, Value::from(method::DISABLED));
        }
    }
    ipv4
}

// ─── NM GetSettings() Parsing Helpers ───

/// Extract a string value from NM connection settings section.
#[inline]
fn settings_str(section: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    section
        .get(key)
        .and_then(|v| v.downcast_ref::<&str>().ok().map(|s| s.to_string()))
}

/// Extract a boolean value from NM connection settings section.
#[inline]
fn settings_bool(section: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    section.get(key).and_then(|v| v.downcast_ref::<bool>().ok())
}

/// Extract a u64 value from NM connection settings section.
#[inline]
fn settings_u64(section: &HashMap<String, OwnedValue>, key: &str) -> Option<u64> {
    section.get(key).and_then(|v| v.downcast_ref::<u64>().ok())
}

/// Parse `address-data` from NM connection settings (ipv4 section).
///
/// Returns (ip_address, prefix_length) of the first address entry.
fn parse_settings_address_data(ipv4_section: &HashMap<String, OwnedValue>) -> Option<(IpAddr, u8)> {
    let addr_data = ipv4_section.get(conn::ADDRESS_DATA)?;
    let arr = addr_data.downcast_ref::<&zbus::zvariant::Array>().ok()?;
    let first = arr.iter().next()?;
    let dict = first.downcast_ref::<&zbus::zvariant::Dict>().ok()?;
    let address =
        dict_lookup_str(dict, prop::ADDR_KEY_ADDRESS).and_then(|s| s.parse::<IpAddr>().ok())?;
    let prefix = dict_lookup_u32(dict, prop::ADDR_KEY_PREFIX).unwrap_or(24) as u8;
    Some((address, prefix))
}

/// Parse DNS servers from NM connection settings (ipv4 section).
///
/// NM stores DNS in the `dns` key as `Array<UInt32>` — each entry is an IPv4
/// address in **network byte order** (little-endian on LE hosts due to how NM
/// internally stores `in_addr`).
fn parse_settings_dns(ipv4_section: &HashMap<String, OwnedValue>) -> Vec<IpAddr> {
    let Some(dns_val) = ipv4_section.get(conn::DNS) else {
        return Vec::new();
    };
    let Ok(arr) = dns_val.downcast_ref::<&zbus::zvariant::Array>() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            item.downcast_ref::<u32>()
                .ok()
                .map(|n| IpAddr::V4(Ipv4Addr::from(u32::from_ne_bytes(n.to_ne_bytes()))))
        })
        .collect()
}

/// Parse DNS search domains from NM connection settings (ipv4 section).
///
/// NM stores search domains in the `dns-search` key as `Array<String>`.
fn parse_settings_search_domains(ipv4_section: &HashMap<String, OwnedValue>) -> Vec<String> {
    let Some(val) = ipv4_section.get(conn::DNS_SEARCH) else {
        return Vec::new();
    };
    let Ok(arr) = val.downcast_ref::<&zbus::zvariant::Array>() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| item.downcast_ref::<&str>().ok().map(|s| s.to_string()))
        .collect()
}

/// Convert `IpAddr` slice to NM's `dns` format (`Array<UInt32>`, network byte order).
///
/// IPv6 addresses are silently skipped — NM's `ipv4.dns` only accepts v4.
fn ip_addrs_to_nm_dns(addrs: &[IpAddr]) -> Vec<u32> {
    addrs
        .iter()
        .filter_map(|addr| match addr {
            IpAddr::V4(v4) => Some(u32::from_ne_bytes(v4.octets())),
            IpAddr::V6(_) => None,
        })
        .collect()
}

// ─── Device Type / State Mapping ───

/// Map NM DeviceType to our InterfaceKind.
#[inline]
fn nm_device_type_to_kind(device_type: u32) -> InterfaceKind {
    match device_type {
        device_type::ETHERNET => InterfaceKind::Ethernet,
        device_type::WIFI => InterfaceKind::Wifi,
        device_type::BRIDGE => InterfaceKind::Bridge,
        device_type::VLAN => InterfaceKind::Vlan,
        14 => InterfaceKind::Loopback, // NM_DEVICE_TYPE_GENERIC (lo)
        _ => InterfaceKind::Unknown,
    }
}

/// Map NM device state to our LinkState.
#[inline]
fn nm_state_to_link_state(state: u32) -> LinkState {
    match state {
        device_state::ACTIVATED => LinkState::Up,
        device_state::DISCONNECTED => LinkState::Down,
        device_state::UNAVAILABLE => LinkState::Down,
        device_state::UNMANAGED => LinkState::Down,
        40..=99 => LinkState::Dormant, // Connecting states
        _ => LinkState::Unknown,
    }
}

/// Map NM Wi-Fi mode constant to our WifiMode.
#[inline]
fn nm_wifi_mode_to_mode(mode: u32) -> WifiMode {
    match mode {
        2 => WifiMode::Station, // NM_802_11_MODE_INFRA
        3 => WifiMode::Ap,      // NM_802_11_MODE_AP
        1 => WifiMode::AdHoc,   // NM_802_11_MODE_ADHOC
        _ => WifiMode::Unknown,
    }
}

/// Derive WifiSecurity from NM AP flags.
#[inline]
fn derive_security(flags: u32, wpa_flags: u32, rsn_flags: u32) -> WifiSecurity {
    if rsn_flags & ap_sec::KEY_MGMT_SAE != 0 {
        return WifiSecurity::Wpa3Sae;
    }
    if rsn_flags & ap_sec::KEY_MGMT_802_1X != 0 {
        return WifiSecurity::Wpa2Enterprise;
    }
    if rsn_flags & ap_sec::KEY_MGMT_PSK != 0 {
        return WifiSecurity::Wpa2Psk;
    }
    if wpa_flags & ap_sec::KEY_MGMT_802_1X != 0 {
        return WifiSecurity::WpaEnterprise;
    }
    if wpa_flags & ap_sec::KEY_MGMT_PSK != 0 {
        return WifiSecurity::WpaPsk;
    }
    if flags & 0x1 != 0 {
        // NM_802_11_AP_FLAGS_PRIVACY — WEP or similar
        return WifiSecurity::Wep;
    }
    WifiSecurity::Open
}

/// Convert frequency (MHz) to channel number.
#[inline]
fn frequency_to_channel(freq: u32) -> u32 {
    match freq {
        2412..=2484 => {
            if freq == 2484 {
                14
            } else {
                (freq - 2407) / 5
            }
        }
        5180..=5825 => (freq - 5000) / 5,
        _ => 0,
    }
}

/// Convert frequency (MHz) to Wi-Fi band.
#[inline]
fn frequency_to_band(freq: u32) -> WifiBand {
    match freq {
        2400..=2500 => WifiBand::Band2_4Ghz,
        5150..=5900 => WifiBand::Band5Ghz,
        5925..=7125 => WifiBand::Band6Ghz,
        _ => WifiBand::Unknown,
    }
}

/// Approximate RSSI (dBm) from NM signal quality (0-100).
#[inline]
fn quality_to_rssi(quality: u8) -> i32 {
    -90 + (quality as i32 * 60 / 100)
}

// ─── D-Bus Property Extraction Helpers ───

/// Trait for extracting typed values from NM property maps.
///
/// Centralizes the repetitive `downcast_ref` pattern into a single generic lookup.
trait NmPropExtract: Sized {
    fn extract(value: &OwnedValue) -> Option<Self>;
}

impl NmPropExtract for String {
    fn extract(value: &OwnedValue) -> Option<Self> {
        value.downcast_ref::<&str>().ok().map(|s| s.to_string())
    }
}

impl NmPropExtract for u32 {
    fn extract(value: &OwnedValue) -> Option<Self> {
        value.downcast_ref::<u32>().ok()
    }
}

impl NmPropExtract for u8 {
    fn extract(value: &OwnedValue) -> Option<Self> {
        value.downcast_ref::<u8>().ok()
    }
}

impl NmPropExtract for i64 {
    fn extract(value: &OwnedValue) -> Option<Self> {
        value.downcast_ref::<i64>().ok()
    }
}

/// Generic D-Bus property lookup from a `HashMap<String, OwnedValue>`.
#[inline]
fn prop<T: NmPropExtract>(props: &HashMap<String, OwnedValue>, key: &str) -> Option<T> {
    props.get(key).and_then(T::extract)
}

/// Convenience aliases for backward compatibility and readability.
#[inline]
fn prop_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    prop::<String>(props, key)
}

#[inline]
fn prop_u32(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    prop::<u32>(props, key)
}

#[inline]
fn prop_u8(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    prop::<u8>(props, key)
}

#[inline]
fn prop_byte_array(props: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<u8>> {
    props.get(key).and_then(|v| {
        let arr = v.downcast_ref::<&zbus::zvariant::Array>().ok()?;
        let bytes: Vec<u8> = arr
            .iter()
            .filter_map(|item| item.downcast_ref::<u8>().ok())
            .collect();
        if bytes.is_empty() {
            None
        } else {
            Some(bytes)
        }
    })
}

#[inline]
fn prop_object_path(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props.get(key).and_then(|v| {
        v.downcast_ref::<&ObjectPath<'_>>()
            .ok()
            .map(|p| p.to_string())
    })
}

/// Look up a key in a zvariant Dict by iterating entries.
///
/// `Dict::get` has complex lifetime constraints; iteration is O(n) but NM dicts are tiny.
#[inline]
fn dict_lookup_str(dict: &zbus::zvariant::Dict<'_, '_>, key: &str) -> Option<String> {
    dict.iter()
        .find(|(k, _)| k.downcast_ref::<&str>().ok() == Some(key))
        .and_then(|(_, v)| v.downcast_ref::<&str>().ok().map(|s| s.to_string()))
}

#[inline]
fn dict_lookup_u32(dict: &zbus::zvariant::Dict<'_, '_>, key: &str) -> Option<u32> {
    dict.iter()
        .find(|(k, _)| k.downcast_ref::<&str>().ok() == Some(key))
        .and_then(|(_, v)| v.downcast_ref::<u32>().ok())
}

/// Parse NM IP4Config AddressData property.
///
/// NM >= 1.0 exposes `AddressData` as `aa{sv}` with "address" (string) and "prefix" (uint32).
fn parse_nm_ip4_addresses(props: &HashMap<String, OwnedValue>) -> Vec<Ipv4AddressInfo> {
    let mut result = Vec::new();

    if let Some(addr_data) = props.get(prop::ADDRESS_DATA) {
        if let Ok(arr) = addr_data.downcast_ref::<&zbus::zvariant::Array>() {
            for item in arr.iter() {
                if let Ok(dict) = item.downcast_ref::<&zbus::zvariant::Dict>() {
                    let address = dict_lookup_str(dict, prop::ADDR_KEY_ADDRESS)
                        .and_then(|s| s.parse::<IpAddr>().ok());
                    let prefix = dict_lookup_u32(dict, prop::ADDR_KEY_PREFIX).unwrap_or(24) as u8;

                    if let Some(addr) = address {
                        result.push(Ipv4AddressInfo {
                            address: addr,
                            prefix_length: prefix,
                        });
                    }
                }
            }
        }
    }

    result
}

/// Parse NM IP4Config NameserverData property.
#[inline]
fn parse_nm_ip4_nameservers(props: &HashMap<String, OwnedValue>) -> Vec<IpAddr> {
    let mut result = Vec::new();

    if let Some(ns_data) = props.get(prop::NAMESERVER_DATA) {
        if let Ok(arr) = ns_data.downcast_ref::<&zbus::zvariant::Array>() {
            for item in arr.iter() {
                if let Ok(dict) = item.downcast_ref::<&zbus::zvariant::Dict>() {
                    if let Some(addr) = dict_lookup_str(dict, prop::ADDR_KEY_ADDRESS)
                        .and_then(|s| s.parse::<IpAddr>().ok())
                    {
                        result.push(addr);
                    }
                }
            }
        }
    }

    result
}

/// Parse DNS search domains from the active NM IP4Config object.
///
/// NM exposes `Searches` as `Array<String>` on the `Ip4Config` D-Bus interface.
#[inline]
fn parse_nm_ip4_search_domains(props: &HashMap<String, OwnedValue>) -> Vec<String> {
    let Some(val) = props.get(prop::SEARCHES) else {
        return Vec::new();
    };
    let Ok(arr) = val.downcast_ref::<&zbus::zvariant::Array>() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| item.downcast_ref::<&str>().ok().map(|s| s.to_string()))
        .collect()
}

/// Parse NM IP6Config AddressData property.
#[inline]
fn parse_nm_ip6_addresses(props: &HashMap<String, OwnedValue>) -> Vec<Ipv6AddressInfo> {
    let mut result = Vec::new();

    if let Some(addr_data) = props.get(prop::ADDRESS_DATA) {
        if let Ok(arr) = addr_data.downcast_ref::<&zbus::zvariant::Array>() {
            for item in arr.iter() {
                if let Ok(dict) = item.downcast_ref::<&zbus::zvariant::Dict>() {
                    let address = dict_lookup_str(dict, prop::ADDR_KEY_ADDRESS)
                        .and_then(|s| s.parse::<IpAddr>().ok());
                    let prefix = dict_lookup_u32(dict, prop::ADDR_KEY_PREFIX).unwrap_or(64) as u8;

                    if let Some(addr) = address {
                        result.push(Ipv6AddressInfo {
                            address: addr,
                            prefix_length: prefix,
                        });
                    }
                }
            }
        }
    }

    result
}

/// Parse NM IP6Config NameserverData / Nameservers property.
#[inline]
fn parse_nm_ip6_nameservers(props: &HashMap<String, OwnedValue>) -> Vec<IpAddr> {
    // Try NameserverData first (NM >= 1.14), fallback to Nameservers.
    parse_nm_ip4_nameservers(props) // Same format
}

/// Read a single sysfs statistic for an interface (async, non-blocking).
#[inline]
async fn read_sysfs_stat(iface: &str, stat: &str) -> Option<u64> {
    tokio::fs::read_to_string(sysfs::stat_path(iface, stat))
        .await
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Read Rx/Tx byte counters from `/sys/class/net/<iface>/statistics/`.
#[inline]
async fn read_sysfs_traffic(iface: &str) -> (Option<u64>, Option<u64>) {
    let (rx, tx) = tokio::join!(
        read_sysfs_stat(iface, sysfs::RX_BYTES),
        read_sysfs_stat(iface, sysfs::TX_BYTES),
    );
    (rx, tx)
}

/// Read packet counters from `/sys/class/net/<iface>/statistics/`.
#[inline]
async fn read_sysfs_counters(iface: &str) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    let (rx_packets, tx_packets, rx_errors, tx_errors) = tokio::join!(
        read_sysfs_stat(iface, sysfs::RX_PACKETS),
        read_sysfs_stat(iface, sysfs::TX_PACKETS),
        read_sysfs_stat(iface, sysfs::RX_ERRORS),
        read_sysfs_stat(iface, sysfs::TX_ERRORS),
    );
    (rx_packets, tx_packets, rx_errors, tx_errors)
}

// ─── Configuration File Parsing Helpers ───

/// Parse a `key=value` line from an INI-style config file (hostapd.conf).
#[inline]
fn parse_conf_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            if k.trim() == key {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Parse a `KEY="value"` line from a shell env file (ap-env).
#[inline]
fn parse_env_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            if k.trim() == key {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// Derive the Wi-Fi security type from hostapd.conf content.
#[inline]
fn parse_hostapd_security(conf_content: &str) -> WifiSecurity {
    let wpa_val = parse_conf_value(conf_content, "wpa");
    let key_mgmt = parse_conf_value(conf_content, "wpa_key_mgmt");

    match (wpa_val.as_deref(), key_mgmt.as_deref()) {
        (Some("2"), Some("SAE")) => WifiSecurity::Wpa3Sae,
        (Some("2"), _) => WifiSecurity::Wpa2Psk,
        (Some("1"), _) => WifiSecurity::WpaPsk,
        (Some("3"), _) => WifiSecurity::Wpa2Psk, // mixed mode WPA/WPA2
        (None, _) | (Some("0"), _) => WifiSecurity::Open,
        _ => WifiSecurity::Unknown,
    }
}

/// Count connected clients via hostapd's control interface.
///
/// Sends the `STA-FIRST` / `STA-NEXT` commands to the UNIX socket at
/// `/var/run/hostapd/<iface>`. Returns `None` if the socket is unavailable.
#[inline]
async fn count_hostapd_clients(iface: &str) -> Option<u32> {
    use tokio::net::UnixDatagram;

    let ctrl_path = format!("{}/{iface}", hostapd_ctrl::CTRL_DIR);
    if !Path::new(&ctrl_path).exists() {
        return None;
    }

    let client_path = format!(
        "{}-{}-{}",
        hostapd_ctrl::CLIENT_PATH_PREFIX,
        iface,
        process::id()
    );
    let _ = fs::remove_file(&client_path);

    let sock = UnixDatagram::bind(&client_path).ok()?;
    sock.connect(&ctrl_path).ok()?;

    sock.send(hostapd_ctrl::CMD_STA_FIRST.as_bytes())
        .await
        .ok()?;

    let mut buf = [0u8; 4096];
    let mut count: u32 = 0;
    const MAX_CLIENTS: u32 = 255;

    loop {
        let n = tokio::time::timeout(Duration::from_millis(200), sock.recv(&mut buf))
            .await
            .ok()?
            .ok()?;

        let resp = str::from_utf8(&buf[..n]).unwrap_or("");
        if resp.is_empty() || resp.starts_with(hostapd_ctrl::RESP_FAIL) {
            break;
        }

        count += 1;
        if count >= MAX_CLIENTS {
            break;
        }

        // Extract MAC from first line for STA-NEXT.
        let mac = resp.lines().next().unwrap_or("");
        if mac.is_empty() || !mac.contains(':') {
            break;
        }

        let next_cmd = format!("{} {mac}", hostapd_ctrl::CMD_STA_NEXT);
        if sock.send(next_cmd.as_bytes()).await.is_err() {
            break;
        }
    }

    let _ = fs::remove_file(&client_path);
    Some(count)
}
