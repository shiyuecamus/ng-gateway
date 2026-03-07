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
    platform::{nm_dbus, PlatformNetworkManager},
};
use async_trait::async_trait;
use ng_gateway_error::{network::NetworkError, NGResult};
use ng_gateway_models::domain::prelude::{
    ApMode, ApStatus, ConfigureApRequest, ConfigureDnsRequest, ConfigureInterfaceRequest,
    DnsConfig, InterfaceKind, IpMethod, Ipv4AddressInfo, Ipv4Config, Ipv6AddressInfo, Ipv6Config,
    LinkState, NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary,
    PlatformSupport, WifiAccessPoint, WifiBand, WifiConnectPreflight, WifiConnectRequest, WifiMode,
    WifiSecurity, WifiStaStatus, WirelessInterfaceCapability,
};
use std::{collections::HashMap, net::IpAddr, time::Duration};
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
// use `nm_dbus::iface::*` constants inside the macro invocation.

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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StashedStaConnection {
    /// Settings connection path (org.freedesktop.NetworkManager.Settings.Connection).
    connection_path: String,
    /// Wi-Fi device path (org.freedesktop.NetworkManager.Device).
    device_path: String,
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
            .get_all_properties(&device_path_ref, nm_dbus::iface::DEVICE)
            .await?;

        let iface_name = prop_str(&dev_props, nm_dbus::prop::INTERFACE).unwrap_or_default();
        if iface_name.is_empty() {
            return Ok(None);
        }

        let device_type = prop_u32(&dev_props, nm_dbus::prop::DEVICE_TYPE).unwrap_or(0);
        let kind = nm_device_type_to_kind(device_type);

        // Skip loopback and unrecognized virtual interfaces.
        if kind == InterfaceKind::Loopback {
            return Ok(None);
        }

        let state = prop_u32(&dev_props, nm_dbus::prop::STATE).unwrap_or(0);
        let link_state = nm_state_to_link_state(state);

        let mac_address = prop_str(&dev_props, nm_dbus::prop::HW_ADDRESS);
        let speed_mbps =
            prop_u32(&dev_props, nm_dbus::prop::SPEED)
                .and_then(|s| if s > 0 { Some(s) } else { None });
        let _mtu = prop_u32(&dev_props, nm_dbus::prop::MTU);
        let _driver = prop_str(&dev_props, nm_dbus::prop::DRIVER);

        // IPv4 config
        let ipv4 = if state == nm_dbus::device_state::ACTIVATED {
            self.read_ipv4_config(&dev_props).await.ok()
        } else {
            None
        };

        // IPv6 config
        let ipv6 = if state == nm_dbus::device_state::ACTIVATED {
            self.read_ipv6_config(&dev_props).await.ok()
        } else {
            None
        };

        // Wi-Fi specific properties
        let (wifi_mode, connected_ssid, ap_ssid, signal_dbm, signal_quality) =
            if kind == InterfaceKind::Wifi && state >= nm_dbus::device_state::DISCONNECTED {
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
        let config_path = prop_object_path(dev_props, nm_dbus::prop::IP4_CONFIG)
            .ok_or(NetworkError::ConfigError("No Ip4Config path".to_string()))?;

        if config_path.as_str() == "/" {
            return Err(NetworkError::ConfigError("Ip4Config is /".to_string()).into());
        }

        let config_path_ref = ObjectPath::try_from(config_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid Ip4Config path: {e}")))?;

        let ip4_props = self
            .get_all_properties(&config_path_ref, nm_dbus::iface::IP4_CONFIG)
            .await?;

        let addresses = parse_nm_ip4_addresses(&ip4_props);
        let gateway =
            prop_str(&ip4_props, nm_dbus::prop::GATEWAY).and_then(|s| s.parse::<IpAddr>().ok());
        let dns = parse_nm_ip4_nameservers(&ip4_props);

        let method = self.resolve_ip_method(dev_props, nm_dbus::conn::IPV4).await;

        Ok(Ipv4Config {
            addresses,
            gateway,
            dns,
            method,
        })
    }

    /// Read IPv6 configuration from the device's Ip6Config object.
    async fn read_ipv6_config(
        &self,
        dev_props: &HashMap<String, OwnedValue>,
    ) -> NGResult<Ipv6Config> {
        let config_path = prop_object_path(dev_props, nm_dbus::prop::IP6_CONFIG)
            .ok_or(NetworkError::ConfigError("No Ip6Config path".to_string()))?;

        if config_path.as_str() == "/" {
            return Err(NetworkError::ConfigError("Ip6Config is /".to_string()).into());
        }

        let config_path_ref = ObjectPath::try_from(config_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid Ip6Config path: {e}")))?;

        let ip6_props = self
            .get_all_properties(&config_path_ref, nm_dbus::iface::IP6_CONFIG)
            .await?;

        let addresses = parse_nm_ip6_addresses(&ip6_props);
        let gateway =
            prop_str(&ip6_props, nm_dbus::prop::GATEWAY).and_then(|s| s.parse::<IpAddr>().ok());
        let dns = parse_nm_ip6_nameservers(&ip6_props);

        let method = self.resolve_ip_method(dev_props, nm_dbus::conn::IPV6).await;

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
        let active_conn_path = match prop_object_path(dev_props, nm_dbus::prop::ACTIVE_CONNECTION)
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
            .get_all_properties(&active_ref, nm_dbus::iface::ACTIVE_CONN)
            .await
            .unwrap_or_default();

        let settings_path = match prop_object_path(&active_props, nm_dbus::prop::CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")
        {
            Some(p) => p,
            None => return IpMethod::Dhcp,
        };

        // Call GetSettings() on the connection to read ipv4/ipv6.method.
        let method_str = self
            .dbus_conn
            .call_method(
                Some(nm_dbus::iface::NM),
                settings_path.as_str(),
                Some(nm_dbus::iface::SETTINGS_CONN),
                nm_dbus::dbus_method::GET_SETTINGS,
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
                let method_val = ip_section.get(nm_dbus::conn::METHOD)?;
                method_val
                    .downcast_ref::<&str>()
                    .ok()
                    .map(|s| s.to_string())
            });

        match method_str.as_deref() {
            Some(nm_dbus::method::MANUAL) => IpMethod::Static,
            Some(nm_dbus::method::DISABLED) => IpMethod::Disabled,
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
            .get_all_properties(device_path, nm_dbus::iface::DEVICE_WIRELESS)
            .await?;

        let mode = prop_u32(&wifi_props, nm_dbus::prop::MODE).map(nm_wifi_mode_to_mode);

        let mut connected_ssid = None;
        let mut signal_dbm = None;
        let mut signal_quality = None;

        if device_state == nm_dbus::device_state::ACTIVATED {
            if let Some(active_ap_path) =
                prop_object_path(&wifi_props, nm_dbus::prop::ACTIVE_ACCESS_POINT)
            {
                if active_ap_path.as_str() != "/" {
                    let ap_path_ref = ObjectPath::try_from(active_ap_path.as_str())
                        .map_err(|e| NetworkError::DBusError(format!("Invalid AP path: {e}")))?;
                    let ap_props = self
                        .get_all_properties(&ap_path_ref, nm_dbus::iface::ACCESS_POINT)
                        .await?;

                    connected_ssid = prop_byte_array(&ap_props, nm_dbus::prop::SSID)
                        .map(|bytes| String::from_utf8_lossy(&bytes).to_string());

                    let strength = prop_u8(&ap_props, nm_dbus::prop::STRENGTH).unwrap_or(0);
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
                .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE)
                .await?;

            let device_type = prop_u32(&dev_props, nm_dbus::prop::DEVICE_TYPE).unwrap_or(0);
            if device_type != nm_dbus::device_type::WIFI {
                continue;
            }

            if let Some(target) = interface_name {
                let name = prop_str(&dev_props, nm_dbus::prop::INTERFACE).unwrap_or_default();
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
                Some(nm_dbus::iface::NM),
                settings_path.as_str(),
                Some(nm_dbus::iface::SETTINGS_CONN),
                nm_dbus::dbus_method::DELETE,
                &(),
            )
            .await;
    }

    /// Find an existing NM connection profile (settings path) for the given interface.
    ///
    /// Queries the device's active connection first; if none, searches all saved
    /// connections for one bound to this interface name.
    async fn find_connection_for_interface(&self, iface_name: &str) -> Option<String> {
        let nm = self.nm_proxy().await.ok()?;
        let device_path = nm.get_device_by_ip_iface(iface_name).await.ok()?;
        let dev_path_ref = ObjectPath::try_from(device_path.as_str()).ok()?;
        let dev_props = self
            .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE)
            .await
            .ok()?;

        // Check if there's an active connection on this device.
        let active_conn_path = prop_object_path(&dev_props, nm_dbus::prop::ACTIVE_CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")?;

        let active_ref = ObjectPath::try_from(active_conn_path.as_str()).ok()?;
        let active_props = self
            .get_all_properties(&active_ref, nm_dbus::iface::ACTIVE_CONN)
            .await
            .ok()?;

        prop_object_path(&active_props, nm_dbus::prop::CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")
    }

    /// Stash the current active STA connection for later restore (exclusive mode only).
    ///
    /// Reads the ActiveConnection from the Wi-Fi device, then the Connection property
    /// (settings path) from that ActiveConnection. Returns `None` if no active connection.
    async fn stash_active_sta_connection(&self) -> Option<StashedStaConnection> {
        let dev_path = self.find_wireless_device(None).await.ok()?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str()).ok()?;
        let dev_props = self
            .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE)
            .await
            .ok()?;

        let active_conn_path = prop_object_path(&dev_props, nm_dbus::prop::ACTIVE_CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")?;

        let active_conn_ref = ObjectPath::try_from(active_conn_path.as_str()).ok()?;
        let active_props = self
            .get_all_properties(&active_conn_ref, nm_dbus::iface::ACTIVE_CONN)
            .await
            .ok()?;

        let connection_path = prop_object_path(&active_props, nm_dbus::prop::CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")?;

        Some(StashedStaConnection {
            connection_path,
            device_path: dev_path.to_string(),
        })
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

        let sta_iface =
            parse_env_value(&current_env, "STA_IFACE").unwrap_or_else(|| "wlan0".to_string());
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

        // Also update hostapd.conf interface line.
        let conf_path = format!("{}/{}", AP_CONFIG_DIR, HOSTAPD_CONF_FILE);
        if let Ok(conf_content) = tokio::fs::read_to_string(&conf_path).await {
            let updated_conf = conf_content
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
            let _ = tokio::fs::write(&conf_path, &updated_conf).await;
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

    /// Ensure `ap-env` and `hostapd.conf` reflect the runtime-detected AP mode.
    ///
    /// The AP config files are initially generated by `init-network.sh` at install
    /// time. If the install-time probe gave a different result than the runtime
    /// probe (e.g. install ran before the WiFi driver was fully initialised), the
    /// config files will say Concurrent while the runtime says Exclusive (or vice
    /// versa). This mismatch causes `ap-setup.sh` to take a code path that is
    /// inconsistent with the Rust-side STA disconnect logic, leading to failures.
    ///
    /// This method reads the current `ap-env`, compares `AP_EXCLUSIVE` and
    /// `AP_IFACE` against `caps.ap_mode`, and rewrites the files if they diverge.
    async fn sync_ap_config_with_mode(&self, caps: &NetworkCapabilities) -> NGResult<()> {
        let env_path = format!("{}/{}", AP_CONFIG_DIR, AP_ENV_FILE);
        let current_env = tokio::fs::read_to_string(&env_path)
            .await
            .unwrap_or_default();

        let file_exclusive = parse_env_value(&current_env, "AP_EXCLUSIVE")
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let runtime_exclusive = caps.ap_mode == ApMode::Exclusive;

        if file_exclusive == runtime_exclusive {
            return Ok(());
        }

        // Mismatch detected — rewrite ap-env and hostapd.conf.
        let sta_iface =
            parse_env_value(&current_env, "STA_IFACE").unwrap_or_else(|| "wlan0".to_string());

        let (new_ap_iface, new_exclusive) = if runtime_exclusive {
            (sta_iface.clone(), true)
        } else {
            (format!("{sta_iface}_ap"), false)
        };

        warn!(
            file_exclusive = file_exclusive,
            runtime_exclusive = runtime_exclusive,
            new_ap_iface = %new_ap_iface,
            "AP config/runtime mode mismatch — rewriting ap-env and hostapd.conf"
        );

        // Rewrite ap-env lines.
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
            .map_err(|e| {
                NetworkError::ApError(format!("Failed to rewrite ap-env for mode sync: {e}"))
            })?;

        // Rewrite hostapd.conf interface line.
        let conf_path = format!("{}/{}", AP_CONFIG_DIR, HOSTAPD_CONF_FILE);
        if let Ok(conf_content) = tokio::fs::read_to_string(&conf_path).await {
            let updated_conf = conf_content
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
            tokio::fs::write(&conf_path, &updated_conf)
                .await
                .map_err(|e| {
                    NetworkError::ApError(format!(
                        "Failed to rewrite hostapd.conf for mode sync: {e}"
                    ))
                })?;
        }

        // Also update dnsmasq-ap.conf interface line.
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
                    NetworkError::ApError(format!(
                        "Failed to rewrite dnsmasq-ap.conf for mode sync: {e}"
                    ))
                })?;
        }

        info!(
            ap_iface = %new_ap_iface,
            exclusive = new_exclusive,
            "AP config files synchronized with runtime mode"
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
            .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE)
            .await?;

        let mtu = prop_u32(&dev_props, nm_dbus::prop::MTU);
        let driver = prop_str(&dev_props, nm_dbus::prop::DRIVER);
        let firmware_version = prop_str(&dev_props, "FirmwareVersion").filter(|v| !v.is_empty());

        // Read NM connection UUID from active connection if present.
        let nm_connection_uuid = prop_object_path(&dev_props, nm_dbus::prop::ACTIVE_CONNECTION)
            .filter(|p| !p.is_empty() && p != "/")
            .and_then(|active_path| {
                // Will be resolved asynchronously below.
                Some(active_path)
            });

        let nm_connection_uuid = if let Some(active_path) = nm_connection_uuid {
            let active_ref = ObjectPath::try_from(active_path.as_str()).ok();
            if let Some(active_ref) = active_ref {
                self.get_all_properties(&active_ref, nm_dbus::iface::ACTIVE_CONN)
                    .await
                    .ok()
                    .and_then(|props| prop_str(&props, nm_dbus::prop::UUID))
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
                    .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE)
                    .await
                    .unwrap_or_default();

                if prop_u32(&dev_props, nm_dbus::prop::DEVICE_TYPE)
                    != Some(nm_dbus::device_type::WIFI)
                {
                    continue;
                }

                let iface_name = prop_str(&dev_props, nm_dbus::prop::INTERFACE).unwrap_or_default();

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
            arch: std::env::consts::ARCH.to_string(),
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
        info!(interface = name, method = ?config.method, "Configuring interface");

        let nm = self.nm_proxy().await?;

        // Resolve device path directly by interface name instead of iterating all devices.
        let device_path = nm
            .get_device_by_ip_iface(name)
            .await
            .map_err(|_| NetworkError::InterfaceNotFound(name.to_string()))?;

        // Pre-compute all owned strings so they outlive the Value borrows.
        let conn_id = format!("{name}-config");
        let addr_str = config.ip_address.as_ref().map(|ip| ip.to_string());
        let gw_str = config.gateway.as_ref().map(|gw| gw.to_string());
        let dns_strings: Vec<String> = config
            .dns
            .as_ref()
            .map(|list| list.iter().map(|d| d.to_string()).collect())
            .unwrap_or_default();

        // Build the NM connection settings dict.
        // All string references borrow from the locals above, so lifetimes are unified.
        let build_settings = || -> HashMap<&str, HashMap<&str, Value<'_>>> {
            let mut conn_settings: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();

            let mut connection: HashMap<&str, Value<'_>> = HashMap::new();
            connection.insert(nm_dbus::conn::ID, Value::from(conn_id.as_str()));
            connection.insert(nm_dbus::conn::TYPE, Value::from(nm_dbus::conn::ETHERNET));
            connection.insert(nm_dbus::conn::INTERFACE_NAME, Value::from(name));
            connection.insert(nm_dbus::conn::AUTOCONNECT, Value::from(true));
            conn_settings.insert(nm_dbus::conn::CONNECTION, connection);

            let mut ipv4: HashMap<&str, Value<'_>> = HashMap::new();
            match config.method {
                IpMethod::Dhcp => {
                    ipv4.insert(nm_dbus::conn::METHOD, Value::from(nm_dbus::method::AUTO));
                }
                IpMethod::Static => {
                    ipv4.insert(nm_dbus::conn::METHOD, Value::from(nm_dbus::method::MANUAL));

                    if let (Some(ip_s), Some(prefix)) = (addr_str.as_deref(), config.prefix_length)
                    {
                        let prefix_u32 = prefix as u32;
                        let mut addr_dict: HashMap<&str, Value<'_>> = HashMap::new();
                        addr_dict.insert(nm_dbus::prop::ADDR_KEY_ADDRESS, Value::from(ip_s));
                        addr_dict.insert(nm_dbus::prop::ADDR_KEY_PREFIX, Value::from(prefix_u32));
                        ipv4.insert(nm_dbus::conn::ADDRESS_DATA, Value::from(vec![addr_dict]));
                    }

                    if let Some(gw) = gw_str.as_deref() {
                        ipv4.insert("gateway", Value::from(gw));
                    }

                    if !dns_strings.is_empty() {
                        let dns_data: Vec<HashMap<&str, Value<'_>>> = dns_strings
                            .iter()
                            .map(|d| {
                                let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                                m.insert(nm_dbus::prop::ADDR_KEY_ADDRESS, Value::from(d.as_str()));
                                m
                            })
                            .collect();
                        ipv4.insert(nm_dbus::conn::DNS_DATA, Value::from(dns_data));
                    }
                }
                IpMethod::Disabled => {
                    ipv4.insert(
                        nm_dbus::conn::METHOD,
                        Value::from(nm_dbus::method::DISABLED),
                    );
                }
            }
            conn_settings.insert(nm_dbus::conn::IPV4, ipv4);

            let mut ipv6: HashMap<&str, Value<'_>> = HashMap::new();
            ipv6.insert(nm_dbus::conn::METHOD, Value::from(nm_dbus::method::AUTO));
            conn_settings.insert(nm_dbus::conn::IPV6, ipv6);

            conn_settings
        };

        // Try to find and update an existing NM connection for this interface
        // to avoid creating duplicate connection profiles.
        if let Some(settings_path) = self.find_connection_for_interface(name).await {
            debug!(interface = name, path = %settings_path, "Updating existing NM connection");

            let settings = build_settings();

            // Update the existing connection via Settings.Connection.Update()
            if let Err(e) = self
                .dbus_conn
                .call_method(
                    Some(nm_dbus::iface::NM),
                    settings_path.as_str(),
                    Some(nm_dbus::iface::SETTINGS_CONN),
                    nm_dbus::dbus_method::UPDATE,
                    &(settings,),
                )
                .await
            {
                warn!(error = %e, "Failed to update existing connection, falling back to AddAndActivate");
            } else {
                // Reactivate the updated connection.
                let settings_ref = ObjectPath::try_from(settings_path.as_str())
                    .map_err(|e| NetworkError::DBusError(format!("Invalid path: {e}")))?;
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
        }

        // Fallback: create a new connection (first time configuration).
        let settings = build_settings();
        let device_path_ref = ObjectPath::try_from(device_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;
        let root_path = ObjectPath::try_from("/")
            .map_err(|e| NetworkError::DBusError(format!("Invalid root path: {e}")))?;

        nm.add_and_activate_connection(settings, &device_path_ref, &root_path)
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

        // Trigger a scan.
        let scan_options: HashMap<&str, Value<'_>> = HashMap::new();
        let wireless_iface = nm_dbus::iface::DEVICE_WIRELESS;

        // Call RequestScan
        let proxy = self.props_proxy(&dev_path_ref).await?;
        let _: () = proxy
            .inner()
            .connection()
            .call_method(
                Some(nm_dbus::iface::NM),
                dev_path.as_str(),
                Some(wireless_iface),
                nm_dbus::dbus_method::REQUEST_SCAN,
                &(scan_options,),
            )
            .await
            .map(|m| m.body().deserialize::<()>().unwrap_or(()))
            .map_err(|e| NetworkError::WifiScanFailed(format!("RequestScan failed: {e}")))?;

        // Poll for scan completion with adaptive backoff instead of a fixed 2s sleep.
        // NM updates LastScan timestamp when the scan finishes; we poll until it changes
        // or a timeout is reached.
        let scan_timeout = Duration::from_secs(5);
        let scan_start = std::time::Instant::now();
        let mut poll_interval = Duration::from_millis(200);

        let last_scan_before: i64 = self
            .get_property(&dev_path_ref, wireless_iface, nm_dbus::prop::LAST_SCAN)
            .await
            .ok()
            .and_then(|v| v.downcast_ref::<i64>().ok())
            .unwrap_or(-1);

        loop {
            sleep(poll_interval).await;

            let last_scan_now: i64 = self
                .get_property(&dev_path_ref, wireless_iface, nm_dbus::prop::LAST_SCAN)
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
                Some(nm_dbus::iface::NM),
                dev_path.as_str(),
                Some(wireless_iface),
                nm_dbus::dbus_method::GET_ALL_ACCESS_POINTS,
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
        let active_ap = prop_object_path(&wifi_props, nm_dbus::prop::ACTIVE_ACCESS_POINT)
            .filter(|p| p.as_str() != "/");

        let mut results = Vec::with_capacity(ap_paths.len());

        for ap_path in &ap_paths {
            let ap_path_ref = match ObjectPath::try_from(ap_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let ap_props = match self
                .get_all_properties(&ap_path_ref, nm_dbus::iface::ACCESS_POINT)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    debug!("Failed to read AP {}: {e}", ap_path.as_str());
                    continue;
                }
            };

            let ssid = prop_byte_array(&ap_props, nm_dbus::prop::SSID)
                .map(|b| String::from_utf8_lossy(&b).to_string())
                .unwrap_or_default();

            let bssid = prop_str(&ap_props, nm_dbus::prop::HW_ADDRESS).unwrap_or_default();
            let strength = prop_u8(&ap_props, nm_dbus::prop::STRENGTH).unwrap_or(0);
            let frequency = prop_u32(&ap_props, nm_dbus::prop::FREQUENCY).unwrap_or(0);
            let max_bitrate = prop_u32(&ap_props, nm_dbus::prop::MAX_BITRATE);
            let flags = prop_u32(&ap_props, nm_dbus::prop::FLAGS).unwrap_or(0);
            let wpa_flags = prop_u32(&ap_props, nm_dbus::prop::WPA_FLAGS).unwrap_or(0);
            let rsn_flags = prop_u32(&ap_props, nm_dbus::prop::RSN_FLAGS).unwrap_or(0);

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
        connection.insert(nm_dbus::conn::ID, Value::from(request.ssid.as_str()));
        connection.insert(nm_dbus::conn::TYPE, Value::from(nm_dbus::conn::WIFI));
        connection.insert(nm_dbus::conn::AUTOCONNECT, Value::from(true));
        conn_settings.insert(nm_dbus::conn::CONNECTION, connection);

        let mut wireless: HashMap<&str, Value<'_>> = HashMap::new();
        let ssid_bytes: Vec<u8> = request.ssid.as_bytes().to_vec();
        wireless.insert(nm_dbus::conn::WIFI_SSID, Value::from(ssid_bytes));
        wireless.insert(
            nm_dbus::conn::WIFI_MODE,
            Value::from(nm_dbus::wifi_mode::INFRASTRUCTURE),
        );
        if request.hidden.unwrap_or(false) {
            wireless.insert(nm_dbus::conn::WIFI_HIDDEN, Value::from(true));
        }
        if let Some(bssid) = &request.bssid {
            wireless.insert(nm_dbus::conn::WIFI_BSSID, Value::from(bssid.as_str()));
        }
        conn_settings.insert(nm_dbus::conn::WIFI, wireless);

        if let Some(password) = &request.password {
            if !password.is_empty() {
                let mut security: HashMap<&str, Value<'_>> = HashMap::new();
                security.insert(
                    nm_dbus::conn::KEY_MGMT,
                    Value::from(nm_dbus::key_mgmt::WPA_PSK),
                );
                security.insert(nm_dbus::conn::PSK, Value::from(password.as_str()));
                conn_settings.insert(nm_dbus::conn::WIFI_SECURITY, security);
            }
        }

        let mut ipv4: HashMap<&str, Value<'_>> = HashMap::new();
        ipv4.insert(nm_dbus::conn::METHOD, Value::from(nm_dbus::method::AUTO));
        conn_settings.insert(nm_dbus::conn::IPV4, ipv4);

        let mut ipv6: HashMap<&str, Value<'_>> = HashMap::new();
        ipv6.insert(nm_dbus::conn::METHOD, Value::from(nm_dbus::method::AUTO));
        conn_settings.insert(nm_dbus::conn::IPV6, ipv6);

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
        let start = std::time::Instant::now();
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
                .get_property(
                    &active_path_ref,
                    nm_dbus::iface::ACTIVE_CONN,
                    nm_dbus::prop::STATE,
                )
                .await
                .ok()
                .and_then(|v| v.downcast_ref::<u32>().ok())
                .unwrap_or(0);

            match state {
                nm_dbus::active_conn_state::ACTIVATED => {
                    info!(ssid = %request.ssid, "Wi-Fi connection established");
                    break Ok(());
                }
                nm_dbus::active_conn_state::DEACTIVATED => {
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

    async fn disconnect_wifi(&self, interface_name: Option<&str>) -> NGResult<()> {
        info!("Disconnecting Wi-Fi STA");

        let dev_path = self.find_wireless_device(interface_name).await?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        let dev_props = self
            .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE)
            .await?;

        let active_conn =
            prop_object_path(&dev_props, nm_dbus::prop::ACTIVE_CONNECTION).filter(|p| p != "/");

        if let Some(conn_path) = active_conn {
            let nm = self.nm_proxy().await?;
            let conn_path_ref = ObjectPath::try_from(conn_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid connection path: {e}")))?;
            nm.deactivate_connection(&conn_path_ref)
                .await
                .map_err(|e| NetworkError::WifiError(format!("Failed to deactivate: {e}")))?;
            info!("Wi-Fi disconnected");
        } else {
            debug!("No active Wi-Fi connection to disconnect");
        }

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
            .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE)
            .await?;

        let iface_name = prop_str(&dev_props, nm_dbus::prop::INTERFACE);
        let state = prop_u32(&dev_props, nm_dbus::prop::STATE).unwrap_or(0);
        let speed = prop_u32(&dev_props, nm_dbus::prop::SPEED);

        if state != nm_dbus::device_state::ACTIVATED {
            return Ok(WifiStaStatus {
                interface_name: iface_name,
                speed_mbps: speed,
                ..Default::default()
            });
        }

        // Read active AP properties.
        let wifi_props = self
            .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE_WIRELESS)
            .await
            .unwrap_or_default();

        let (ssid, bssid, security, band, channel, frequency, signal_dbm, signal_quality) =
            if let Some(ap_path) = prop_object_path(&wifi_props, nm_dbus::prop::ACTIVE_ACCESS_POINT)
                .filter(|p| p != "/")
            {
                let ap_path_ref = ObjectPath::try_from(ap_path.as_str())
                    .map_err(|e| NetworkError::DBusError(format!("Invalid AP path: {e}")))?;
                let ap_props = self
                    .get_all_properties(&ap_path_ref, nm_dbus::iface::ACCESS_POINT)
                    .await
                    .unwrap_or_default();

                let ssid = prop_byte_array(&ap_props, nm_dbus::prop::SSID)
                    .map(|b| String::from_utf8_lossy(&b).to_string());
                let bssid = prop_str(&ap_props, nm_dbus::prop::HW_ADDRESS);
                let strength = prop_u8(&ap_props, nm_dbus::prop::STRENGTH).unwrap_or(0);
                let freq = prop_u32(&ap_props, nm_dbus::prop::FREQUENCY).unwrap_or(0);
                let flags = prop_u32(&ap_props, nm_dbus::prop::FLAGS).unwrap_or(0);
                let wpa_flags = prop_u32(&ap_props, nm_dbus::prop::WPA_FLAGS).unwrap_or(0);
                let rsn_flags = prop_u32(&ap_props, nm_dbus::prop::RSN_FLAGS).unwrap_or(0);

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
                sta_restore_failed: false,
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

        let frequency = channel.map(|ch| {
            if ch <= 14 {
                2407 + ch * 5
            } else {
                5000 + ch * 5
            }
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
            sta_restore_failed: false,
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

        // Synchronize ap-env and hostapd.conf with the runtime-detected AP mode.
        // This prevents the mismatch where Rust detects Exclusive but ap-env says
        // Concurrent (or vice versa), which causes ap-setup.sh to take a code path
        // inconsistent with the Rust-side STA disconnect logic.
        self.sync_ap_config_with_mode(&caps).await?;

        // In exclusive mode, stash STA connection for restore after stop_ap, then disconnect.
        // Persist to disk so gateway restarts don't lose the restore info.
        if caps.ap_mode == ApMode::Exclusive {
            info!("Exclusive mode: stashing STA connection and disconnecting");
            if let Some(stashed) = self.stash_active_sta_connection().await {
                stashed.persist().await;
                let mut guard = self.stashed_sta_for_restore.write().await;
                *guard = Some(stashed);
            }
            if let Err(e) = self.disconnect_wifi(None).await {
                warn!(error = %e, "Failed to disconnect STA (may already be disconnected)");
            }
            sleep(Duration::from_millis(500)).await;
        }

        let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
        mgr.start_all().await?;

        info!("AP hotspot started");
        self.ap_status().await
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
        let stashed = self.stashed_sta_for_restore.write().await.take();
        if let Some(s) = stashed {
            StashedStaConnection::remove().await;

            // Give NM time to recognise the restored managed-mode interface.
            sleep(Duration::from_millis(1500)).await;

            let conn_ref = ObjectPath::try_from(s.connection_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid connection path: {e}")))?;
            let dev_ref = ObjectPath::try_from(s.device_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;
            let root = ObjectPath::try_from("/")
                .map_err(|e| NetworkError::DBusError(format!("Invalid root path: {e}")))?;

            match self
                .nm_proxy()
                .await?
                .activate_connection(&conn_ref, &dev_ref, &root)
                .await
            {
                Ok(_) => {
                    info!("Restored previous Wi-Fi STA connection after stopping AP");
                    sleep(Duration::from_millis(2000)).await;
                }
                Err(e) => {
                    warn!(error = %e, "Failed to restore STA connection after stop_ap");
                }
            }
        }

        info!("AP hotspot stopped");
        self.ap_status().await
    }

    async fn configure_ap(&self, config: &ConfigureApRequest) -> NGResult<ApStatus> {
        info!(ssid = ?config.ssid, channel = ?config.channel, "Configuring AP hotspot");

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

    async fn get_dns(&self) -> NGResult<DnsConfig> {
        // Aggregate DNS from all activated interfaces.
        let nm = self.nm_proxy().await?;
        let devices = nm.get_devices().await.unwrap_or_default();
        let mut all_dns: Vec<IpAddr> = Vec::new();
        let all_domains: Vec<String> = Vec::new();
        let mut method = IpMethod::Dhcp;

        for dev_path in &devices {
            let dev_path_ref = match ObjectPath::try_from(dev_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let dev_props = self
                .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE)
                .await
                .unwrap_or_default();

            let state = prop_u32(&dev_props, nm_dbus::prop::STATE).unwrap_or(0);
            if state != nm_dbus::device_state::ACTIVATED {
                continue;
            }

            if let Ok(ip4) = self.read_ipv4_config(&dev_props).await {
                for d in &ip4.dns {
                    if !all_dns.contains(d) {
                        all_dns.push(*d);
                    }
                }
                if ip4.method == IpMethod::Static {
                    method = IpMethod::Static;
                }
            }
        }

        Ok(DnsConfig {
            servers: all_dns,
            search_domains: all_domains,
            mode: method,
        })
    }

    async fn configure_dns(&self, config: &ConfigureDnsRequest) -> NGResult<()> {
        info!(servers = ?config.servers, "Configuring global DNS");

        // NM global DNS is set via the main NM settings.
        // The simplest approach: update the first active wired connection's DNS.
        let nm = self.nm_proxy().await?;
        let devices = nm
            .get_devices()
            .await
            .map_err(|e| NetworkError::DBusError(format!("GetDevices failed: {e}")))?;

        // Find first activated ethernet device.
        for dev_path in &devices {
            let dev_path_ref = match ObjectPath::try_from(dev_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let dev_props = self
                .get_all_properties(&dev_path_ref, nm_dbus::iface::DEVICE)
                .await
                .unwrap_or_default();

            let device_type = prop_u32(&dev_props, nm_dbus::prop::DEVICE_TYPE).unwrap_or(0);
            let state = prop_u32(&dev_props, nm_dbus::prop::STATE).unwrap_or(0);
            let iface_name = prop_str(&dev_props, nm_dbus::prop::INTERFACE).unwrap_or_default();

            if device_type != nm_dbus::device_type::ETHERNET
                || state != nm_dbus::device_state::ACTIVATED
            {
                continue;
            }

            // Preserve the current IP method so we don't accidentally
            // switch a static-IP interface to DHCP.
            let current_ip4 = self.read_ipv4_config(&dev_props).await.ok();
            let current_method = current_ip4
                .as_ref()
                .map(|c| c.method)
                .unwrap_or(IpMethod::Dhcp);

            let dns_list: Vec<IpAddr> = config
                .servers
                .iter()
                .filter_map(|s| s.to_string().parse::<IpAddr>().ok())
                .collect();

            let (ip_address, prefix_length, gateway) = if current_method == IpMethod::Static {
                let addr = current_ip4
                    .as_ref()
                    .and_then(|c| c.addresses.first())
                    .map(|a| a.address);
                let prefix = current_ip4
                    .as_ref()
                    .and_then(|c| c.addresses.first())
                    .map(|a| a.prefix_length);
                let gw = current_ip4.as_ref().and_then(|c| c.gateway);
                (addr, prefix, gw)
            } else {
                (None, None, None)
            };

            let apply_config = ConfigureInterfaceRequest {
                method: current_method,
                ip_address,
                prefix_length,
                gateway,
                dns: if dns_list.is_empty() {
                    None
                } else {
                    Some(dns_list)
                },
            };

            return self.configure_interface(&iface_name, &apply_config).await;
        }

        Err(NetworkError::DnsError(
            "No active ethernet interface found to apply DNS configuration".to_string(),
        )
        .into())
    }
}

// ─── Helper Functions ───

/// Map NM DeviceType to our InterfaceKind.
#[inline]
fn nm_device_type_to_kind(device_type: u32) -> InterfaceKind {
    match device_type {
        nm_dbus::device_type::ETHERNET => InterfaceKind::Ethernet,
        nm_dbus::device_type::WIFI => InterfaceKind::Wifi,
        nm_dbus::device_type::BRIDGE => InterfaceKind::Bridge,
        nm_dbus::device_type::VLAN => InterfaceKind::Vlan,
        14 => InterfaceKind::Loopback, // NM_DEVICE_TYPE_GENERIC (lo)
        _ => InterfaceKind::Unknown,
    }
}

/// Map NM device state to our LinkState.
#[inline]
fn nm_state_to_link_state(state: u32) -> LinkState {
    match state {
        nm_dbus::device_state::ACTIVATED => LinkState::Up,
        nm_dbus::device_state::DISCONNECTED => LinkState::Down,
        nm_dbus::device_state::UNAVAILABLE => LinkState::Down,
        nm_dbus::device_state::UNMANAGED => LinkState::Down,
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
    if rsn_flags & nm_dbus::ap_sec::KEY_MGMT_SAE != 0 {
        return WifiSecurity::Wpa3Sae;
    }
    if rsn_flags & nm_dbus::ap_sec::KEY_MGMT_802_1X != 0 {
        return WifiSecurity::Wpa2Enterprise;
    }
    if rsn_flags & nm_dbus::ap_sec::KEY_MGMT_PSK != 0 {
        return WifiSecurity::Wpa2Psk;
    }
    if wpa_flags & nm_dbus::ap_sec::KEY_MGMT_802_1X != 0 {
        return WifiSecurity::WpaEnterprise;
    }
    if wpa_flags & nm_dbus::ap_sec::KEY_MGMT_PSK != 0 {
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

    if let Some(addr_data) = props.get(nm_dbus::prop::ADDRESS_DATA) {
        if let Ok(arr) = addr_data.downcast_ref::<&zbus::zvariant::Array>() {
            for item in arr.iter() {
                if let Ok(dict) = item.downcast_ref::<&zbus::zvariant::Dict>() {
                    let address = dict_lookup_str(dict, nm_dbus::prop::ADDR_KEY_ADDRESS)
                        .and_then(|s| s.parse::<IpAddr>().ok());
                    let prefix =
                        dict_lookup_u32(dict, nm_dbus::prop::ADDR_KEY_PREFIX).unwrap_or(24) as u8;

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

    if let Some(ns_data) = props.get(nm_dbus::prop::NAMESERVER_DATA) {
        if let Ok(arr) = ns_data.downcast_ref::<&zbus::zvariant::Array>() {
            for item in arr.iter() {
                if let Ok(dict) = item.downcast_ref::<&zbus::zvariant::Dict>() {
                    if let Some(addr) = dict_lookup_str(dict, nm_dbus::prop::ADDR_KEY_ADDRESS)
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

/// Parse NM IP6Config AddressData property.
#[inline]
fn parse_nm_ip6_addresses(props: &HashMap<String, OwnedValue>) -> Vec<Ipv6AddressInfo> {
    let mut result = Vec::new();

    if let Some(addr_data) = props.get(nm_dbus::prop::ADDRESS_DATA) {
        if let Ok(arr) = addr_data.downcast_ref::<&zbus::zvariant::Array>() {
            for item in arr.iter() {
                if let Ok(dict) = item.downcast_ref::<&zbus::zvariant::Dict>() {
                    let address = dict_lookup_str(dict, nm_dbus::prop::ADDR_KEY_ADDRESS)
                        .and_then(|s| s.parse::<IpAddr>().ok());
                    let prefix =
                        dict_lookup_u32(dict, nm_dbus::prop::ADDR_KEY_PREFIX).unwrap_or(64) as u8;

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
    if !std::path::Path::new(&ctrl_path).exists() {
        return None;
    }

    let client_path = format!(
        "{}-{}-{}",
        hostapd_ctrl::CLIENT_PATH_PREFIX,
        iface,
        std::process::id()
    );
    let _ = std::fs::remove_file(&client_path);

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

        let resp = std::str::from_utf8(&buf[..n]).unwrap_or("");
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

    let _ = std::fs::remove_file(&client_path);
    Some(count)
}
