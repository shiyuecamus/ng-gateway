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
    ap_config::{self, ApRenderContext, AP_CONFIG_DIR},
    ap_manager::ApServiceManager,
    capability::{
        aggregate_sta_ap_capability, detect_phy_capabilities, determine_ap_mode, resolve_phy_name,
    },
    platform::PlatformNetworkManager,
};
use async_trait::async_trait;
use ng_gateway_error::{network::NetworkError, NGResult};
use ng_gateway_models::domain::prelude::{
    ApMode, ApStatus, ConfigureApRequest, ConfigureDnsRequest, ConfigureInterfaceRequest,
    DnsConfig, InterfaceKind, IpMethod, Ipv4AddressInfo, Ipv4Config, Ipv6AddressInfo, Ipv6Config,
    LinkState, NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary,
    PlatformSupport, WifiAccessPoint, WifiBand, WifiConnectRequest, WifiMode, WifiSecurity,
    WifiStaStatus, WirelessInterfaceCapability,
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

// ─── NM Device Type Constants ───

const NM_DEVICE_TYPE_ETHERNET: u32 = 1;
const NM_DEVICE_TYPE_WIFI: u32 = 2;
const NM_DEVICE_TYPE_BRIDGE: u32 = 13;
const NM_DEVICE_TYPE_VLAN: u32 = 11;

// ─── NM Device State Constants ───

const NM_DEVICE_STATE_ACTIVATED: u32 = 100;
const NM_DEVICE_STATE_DISCONNECTED: u32 = 30;
const NM_DEVICE_STATE_UNAVAILABLE: u32 = 20;
const NM_DEVICE_STATE_UNMANAGED: u32 = 10;

/// Stashed STA connection info for restore-after-stop-AP in exclusive mode.
///
/// When starting AP in exclusive mode we disconnect STA; we save the NM connection
/// and device paths so we can reactivate via `ActivateConnection` when the user stops AP.
#[derive(Debug, Clone)]
struct StashedStaConnection {
    /// Settings connection path (org.freedesktop.NetworkManager.Settings.Connection).
    connection_path: String,
    /// Wi-Fi device path (org.freedesktop.NetworkManager.Device).
    device_path: String,
}

/// Linux network manager backed by NetworkManager D-Bus.
pub struct LinuxNetworkManager {
    dbus_conn: Connection,
    /// In exclusive mode, STA is disconnected before AP start; this holds the connection
    /// info for restore when the user stops AP.
    stashed_sta_for_restore: RwLock<Option<StashedStaConnection>>,
}

impl LinuxNetworkManager {
    /// Create a new instance by connecting to the system D-Bus.
    pub async fn new() -> NGResult<Self> {
        let dbus_conn = Connection::system().await.map_err(|e| {
            NetworkError::DBusError(format!("Failed to connect to system D-Bus: {e}"))
        })?;

        info!("Connected to system D-Bus for NetworkManager");
        Ok(Self {
            dbus_conn,
            stashed_sta_for_restore: RwLock::new(None),
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
        device_path: &ObjectPath<'_>,
    ) -> NGResult<Option<NetworkInterfaceSummary>> {
        let dev_props = self
            .get_all_properties(device_path, "org.freedesktop.NetworkManager.Device")
            .await?;

        let iface_name = prop_str(&dev_props, "Interface").unwrap_or_default();
        if iface_name.is_empty() {
            return Ok(None);
        }

        let device_type = prop_u32(&dev_props, "DeviceType").unwrap_or(0);
        let kind = nm_device_type_to_kind(device_type);

        // Skip loopback and unrecognized virtual interfaces.
        if kind == InterfaceKind::Loopback {
            return Ok(None);
        }

        let state = prop_u32(&dev_props, "State").unwrap_or(0);
        let link_state = nm_state_to_link_state(state);

        let mac_address = prop_str(&dev_props, "HwAddress");
        let speed_mbps =
            prop_u32(&dev_props, "Speed").and_then(|s| if s > 0 { Some(s) } else { None });
        let _mtu = prop_u32(&dev_props, "Mtu");
        let _driver = prop_str(&dev_props, "Driver");

        // IPv4 config
        let ipv4 = if state == NM_DEVICE_STATE_ACTIVATED {
            self.read_ipv4_config(&dev_props).await.ok()
        } else {
            None
        };

        // IPv6 config
        let ipv6 = if state == NM_DEVICE_STATE_ACTIVATED {
            self.read_ipv6_config(&dev_props).await.ok()
        } else {
            None
        };

        // Wi-Fi specific properties
        let (wifi_mode, connected_ssid, ap_ssid, signal_dbm, signal_quality) =
            if kind == InterfaceKind::Wifi && state >= NM_DEVICE_STATE_DISCONNECTED {
                self.read_wifi_info(device_path, state)
                    .await
                    .unwrap_or_default()
            } else {
                Default::default()
            };

        // Traffic stats from /sys/class/net
        let (rx_bytes, tx_bytes) = read_sysfs_traffic(&iface_name);

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
    async fn read_ipv4_config(
        &self,
        dev_props: &HashMap<String, OwnedValue>,
    ) -> NGResult<Ipv4Config> {
        let config_path = prop_object_path(dev_props, "Ip4Config")
            .ok_or(NetworkError::ConfigError("No Ip4Config path".to_string()))?;

        if config_path.as_str() == "/" {
            return Err(NetworkError::ConfigError("Ip4Config is /".to_string()).into());
        }

        let config_path_ref = ObjectPath::try_from(config_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid Ip4Config path: {e}")))?;

        let ip4_props = self
            .get_all_properties(&config_path_ref, "org.freedesktop.NetworkManager.IP4Config")
            .await?;

        let addresses = parse_nm_ip4_addresses(&ip4_props);
        let gateway = prop_str(&ip4_props, "Gateway").and_then(|s| s.parse::<IpAddr>().ok());
        let dns = parse_nm_ip4_nameservers(&ip4_props);

        Ok(Ipv4Config {
            addresses,
            gateway,
            dns,
            method: IpMethod::Dhcp,
        })
    }

    /// Read IPv6 configuration from the device's Ip6Config object.
    async fn read_ipv6_config(
        &self,
        dev_props: &HashMap<String, OwnedValue>,
    ) -> NGResult<Ipv6Config> {
        let config_path = prop_object_path(dev_props, "Ip6Config")
            .ok_or(NetworkError::ConfigError("No Ip6Config path".to_string()))?;

        if config_path.as_str() == "/" {
            return Err(NetworkError::ConfigError("Ip6Config is /".to_string()).into());
        }

        let config_path_ref = ObjectPath::try_from(config_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid Ip6Config path: {e}")))?;

        let ip6_props = self
            .get_all_properties(&config_path_ref, "org.freedesktop.NetworkManager.IP6Config")
            .await?;

        let addresses = parse_nm_ip6_addresses(&ip6_props);
        let gateway = prop_str(&ip6_props, "Gateway").and_then(|s| s.parse::<IpAddr>().ok());
        let dns = parse_nm_ip6_nameservers(&ip6_props);

        Ok(Ipv6Config {
            addresses,
            gateway,
            dns,
            method: IpMethod::Dhcp,
        })
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
            .get_all_properties(
                device_path,
                "org.freedesktop.NetworkManager.Device.Wireless",
            )
            .await?;

        let mode = prop_u32(&wifi_props, "Mode").map(nm_wifi_mode_to_mode);

        let mut connected_ssid = None;
        let mut signal_dbm = None;
        let mut signal_quality = None;

        if device_state == NM_DEVICE_STATE_ACTIVATED {
            if let Some(active_ap_path) = prop_object_path(&wifi_props, "ActiveAccessPoint") {
                if active_ap_path.as_str() != "/" {
                    let ap_path_ref = ObjectPath::try_from(active_ap_path.as_str())
                        .map_err(|e| NetworkError::DBusError(format!("Invalid AP path: {e}")))?;
                    let ap_props = self
                        .get_all_properties(
                            &ap_path_ref,
                            "org.freedesktop.NetworkManager.AccessPoint",
                        )
                        .await?;

                    connected_ssid = prop_byte_array(&ap_props, "Ssid")
                        .map(|bytes| String::from_utf8_lossy(&bytes).to_string());

                    let strength = prop_u8(&ap_props, "Strength").unwrap_or(0);
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
                .get_all_properties(&dev_path_ref, "org.freedesktop.NetworkManager.Device")
                .await?;

            let device_type = prop_u32(&dev_props, "DeviceType").unwrap_or(0);
            if device_type != NM_DEVICE_TYPE_WIFI {
                continue;
            }

            if let Some(target) = interface_name {
                let name = prop_str(&dev_props, "Interface").unwrap_or_default();
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

    /// Stash the current active STA connection for later restore (exclusive mode only).
    ///
    /// Reads the ActiveConnection from the Wi-Fi device, then the Connection property
    /// (settings path) from that ActiveConnection. Returns `None` if no active connection.
    async fn stash_active_sta_connection(&self) -> Option<StashedStaConnection> {
        let dev_path = self.find_wireless_device(None).await.ok()?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str()).ok()?;
        let dev_props = self
            .get_all_properties(&dev_path_ref, "org.freedesktop.NetworkManager.Device")
            .await
            .ok()?;

        let active_conn_path = prop_object_path(&dev_props, "ActiveConnection")
            .filter(|p| !p.is_empty() && p != "/")?;

        let active_conn_ref = ObjectPath::try_from(active_conn_path.as_str()).ok()?;
        let active_props = self
            .get_all_properties(
                &active_conn_ref,
                "org.freedesktop.NetworkManager.Connection.Active",
            )
            .await
            .ok()?;

        let connection_path =
            prop_object_path(&active_props, "Connection").filter(|p| !p.is_empty() && p != "/")?;

        Some(StashedStaConnection {
            connection_path,
            device_path: dev_path.to_string(),
        })
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

        let mut interfaces = Vec::with_capacity(devices.len());

        for dev_path in &devices {
            let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

            match self.build_interface_summary(&dev_path_ref).await {
                Ok(Some(summary)) => interfaces.push(summary),
                Ok(None) => {}
                Err(e) => {
                    warn!("Failed to read device {}: {e}", dev_path.as_str());
                }
            }
        }

        // Sort: Ethernet first, then Wi-Fi, then others.
        interfaces.sort_by_key(|i| match i.kind {
            InterfaceKind::Ethernet => 0,
            InterfaceKind::Wifi => 1,
            InterfaceKind::Bridge => 2,
            _ => 3,
        });

        Ok(interfaces)
    }

    async fn get_interface(&self, name: &str) -> NGResult<NetworkInterfaceDetail> {
        let nm = self.nm_proxy().await?;
        let devices = nm
            .get_devices()
            .await
            .map_err(|e| NetworkError::DBusError(format!("GetDevices failed: {e}")))?;

        for dev_path in &devices {
            let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

            let dev_props = self
                .get_all_properties(&dev_path_ref, "org.freedesktop.NetworkManager.Device")
                .await?;

            let iface_name = prop_str(&dev_props, "Interface").unwrap_or_default();
            if iface_name != name {
                continue;
            }

            let summary = self
                .build_interface_summary(&dev_path_ref)
                .await?
                .ok_or(NetworkError::InterfaceNotFound(name.to_string()))?;

            let mtu = prop_u32(&dev_props, "Mtu");
            let driver = prop_str(&dev_props, "Driver");
            let firmware_version = prop_str(&dev_props, "FirmwareVersion").and_then(|v| {
                if v.is_empty() {
                    None
                } else {
                    Some(v)
                }
            });
            let nm_connection_uuid = None; // TODO: read from active connection

            // Read packet counters from sysfs
            let (rx_packets, tx_packets, rx_errors, tx_errors) = read_sysfs_counters(&name);

            return Ok(NetworkInterfaceDetail {
                summary,
                nm_connection_uuid,
                mtu,
                driver,
                firmware_version,
                rx_packets,
                tx_packets,
                rx_errors,
                tx_errors,
            });
        }

        Err(NetworkError::InterfaceNotFound(name.to_string()).into())
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
                    .get_all_properties(&dev_path_ref, "org.freedesktop.NetworkManager.Device")
                    .await
                    .unwrap_or_default();

                if prop_u32(&dev_props, "DeviceType") != Some(NM_DEVICE_TYPE_WIFI) {
                    continue;
                }

                let iface_name = prop_str(&dev_props, "Interface").unwrap_or_default();

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
        let devices = nm
            .get_devices()
            .await
            .map_err(|e| NetworkError::DBusError(format!("GetDevices failed: {e}")))?;

        // Find the device object path for the given interface name.
        let mut target_device: Option<OwnedObjectPath> = None;
        for dev_path in &devices {
            let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
                .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;
            let dev_props = self
                .get_all_properties(&dev_path_ref, "org.freedesktop.NetworkManager.Device")
                .await?;
            let iface_name = prop_str(&dev_props, "Interface").unwrap_or_default();
            if iface_name == name {
                target_device = Some(dev_path.clone());
                break;
            }
        }

        let device_path = target_device.ok_or(NetworkError::InterfaceNotFound(name.to_string()))?;

        // Build NM connection settings dict.
        let mut conn_settings: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();

        // connection section
        let mut connection: HashMap<&str, Value<'_>> = HashMap::new();
        connection.insert("id", Value::from(format!("{name}-config")));
        connection.insert("type", Value::from("802-3-ethernet"));
        connection.insert("interface-name", Value::from(name));
        connection.insert("autoconnect", Value::from(true));
        conn_settings.insert("connection", connection);

        // Pre-compute owned strings so they outlive the Value borrows.
        let addr_str = config.ip_address.as_ref().map(|ip| ip.to_string());
        let gw_str = config.gateway.as_ref().map(|gw| gw.to_string());
        let dns_strings: Vec<String> = config
            .dns
            .as_ref()
            .map(|list| list.iter().map(|d| d.to_string()).collect())
            .unwrap_or_default();

        // ipv4 section
        let mut ipv4: HashMap<&str, Value<'_>> = HashMap::new();
        match config.method {
            IpMethod::Dhcp => {
                ipv4.insert("method", Value::from("auto"));
            }
            IpMethod::Static => {
                ipv4.insert("method", Value::from("manual"));

                if let (Some(ip_s), Some(prefix)) = (addr_str.as_deref(), config.prefix_length) {
                    let prefix_u32 = prefix as u32;

                    // NM AddressData format: aa{sv} with "address" and "prefix" keys
                    let mut addr_dict: HashMap<&str, Value<'_>> = HashMap::new();
                    addr_dict.insert("address", Value::from(ip_s));
                    addr_dict.insert("prefix", Value::from(prefix_u32));

                    ipv4.insert("address-data", Value::from(vec![addr_dict]));
                }

                if let Some(gw) = gw_str.as_deref() {
                    ipv4.insert("gateway", Value::from(gw));
                }

                if !dns_strings.is_empty() {
                    let dns_strs: Vec<&str> = dns_strings.iter().map(|s| s.as_str()).collect();
                    // NM dns format for manual: use dns-data (aa{sv})
                    let dns_data: Vec<HashMap<&str, Value<'_>>> = dns_strs
                        .iter()
                        .map(|d| {
                            let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                            m.insert("address", Value::from(*d));
                            m
                        })
                        .collect();
                    ipv4.insert("dns-data", Value::from(dns_data));
                }
            }
            IpMethod::Disabled => {
                ipv4.insert("method", Value::from("disabled"));
            }
        }
        conn_settings.insert("ipv4", ipv4);

        // ipv6 — keep auto
        let mut ipv6: HashMap<&str, Value<'_>> = HashMap::new();
        ipv6.insert("method", Value::from("auto"));
        conn_settings.insert("ipv6", ipv6);

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
            "Interface configuration applied successfully"
        );
        Ok(())
    }

    async fn scan_wifi(&self, interface_name: Option<&str>) -> NGResult<Vec<WifiAccessPoint>> {
        let dev_path = self.find_wireless_device(interface_name).await?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        // Trigger a scan.
        let scan_options: HashMap<&str, Value<'_>> = HashMap::new();
        let wireless_iface = "org.freedesktop.NetworkManager.Device.Wireless";

        // Call RequestScan
        let proxy = self.props_proxy(&dev_path_ref).await?;
        let _: () = proxy
            .inner()
            .connection()
            .call_method(
                Some("org.freedesktop.NetworkManager"),
                dev_path.as_str(),
                Some(wireless_iface),
                "RequestScan",
                &(scan_options,),
            )
            .await
            .map(|m| m.body().deserialize::<()>().unwrap_or(()))
            .map_err(|e| NetworkError::WifiScanFailed(format!("RequestScan failed: {e}")))?;

        // Brief wait for scan results.
        sleep(Duration::from_secs(2)).await;

        // Read access points.
        let ap_paths: Vec<OwnedObjectPath> = proxy
            .inner()
            .connection()
            .call_method(
                Some("org.freedesktop.NetworkManager"),
                dev_path.as_str(),
                Some(wireless_iface),
                "GetAllAccessPoints",
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
            prop_object_path(&wifi_props, "ActiveAccessPoint").filter(|p| p.as_str() != "/");

        let mut results = Vec::with_capacity(ap_paths.len());

        for ap_path in &ap_paths {
            let ap_path_ref = match ObjectPath::try_from(ap_path.as_str()) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let ap_props = match self
                .get_all_properties(&ap_path_ref, "org.freedesktop.NetworkManager.AccessPoint")
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    debug!("Failed to read AP {}: {e}", ap_path.as_str());
                    continue;
                }
            };

            let ssid = prop_byte_array(&ap_props, "Ssid")
                .map(|b| String::from_utf8_lossy(&b).to_string())
                .unwrap_or_default();

            let bssid = prop_str(&ap_props, "HwAddress").unwrap_or_default();
            let strength = prop_u8(&ap_props, "Strength").unwrap_or(0);
            let frequency = prop_u32(&ap_props, "Frequency").unwrap_or(0);
            let max_bitrate = prop_u32(&ap_props, "MaxBitrate");
            let flags = prop_u32(&ap_props, "Flags").unwrap_or(0);
            let wpa_flags = prop_u32(&ap_props, "WpaFlags").unwrap_or(0);
            let rsn_flags = prop_u32(&ap_props, "RsnFlags").unwrap_or(0);

            let channel = frequency_to_channel(frequency);
            let band = crate::network::platform::linux::frequency_to_band(frequency);
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

        // Sort by signal descending, deduplicate by SSID (keep strongest).
        results.sort_by(|a, b| b.signal_quality.cmp(&a.signal_quality));
        let mut seen = std::collections::HashSet::new();
        results.retain(|ap| {
            if ap.ssid.is_empty() {
                return false;
            }
            seen.insert(ap.ssid.clone())
        });

        Ok(results)
    }

    async fn connect_wifi(&self, request: &WifiConnectRequest) -> NGResult<WifiStaStatus> {
        info!(ssid = %request.ssid, "Connecting to Wi-Fi network");

        let dev_path = self
            .find_wireless_device(request.interface_name.as_deref())
            .await?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        let nm = self.nm_proxy().await?;

        // Build NM connection settings for 802-11-wireless.
        let mut conn_settings: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();

        let mut connection: HashMap<&str, Value<'_>> = HashMap::new();
        connection.insert("id", Value::from(request.ssid.as_str()));
        connection.insert("type", Value::from("802-11-wireless"));
        connection.insert("autoconnect", Value::from(true));
        conn_settings.insert("connection", connection);

        let mut wireless: HashMap<&str, Value<'_>> = HashMap::new();
        let ssid_bytes: Vec<u8> = request.ssid.as_bytes().to_vec();
        wireless.insert("ssid", Value::from(ssid_bytes));
        wireless.insert("mode", Value::from("infrastructure"));
        if request.hidden.unwrap_or(false) {
            wireless.insert("hidden", Value::from(true));
        }
        if let Some(bssid) = &request.bssid {
            wireless.insert("bssid", Value::from(bssid.as_str()));
        }
        conn_settings.insert("802-11-wireless", wireless);

        if let Some(password) = &request.password {
            if !password.is_empty() {
                let mut security: HashMap<&str, Value<'_>> = HashMap::new();
                security.insert("key-mgmt", Value::from("wpa-psk"));
                security.insert("psk", Value::from(password.as_str()));
                conn_settings.insert("802-11-wireless-security", security);
            }
        }

        let mut ipv4: HashMap<&str, Value<'_>> = HashMap::new();
        ipv4.insert("method", Value::from("auto"));
        conn_settings.insert("ipv4", ipv4);

        let mut ipv6: HashMap<&str, Value<'_>> = HashMap::new();
        ipv6.insert("method", Value::from("auto"));
        conn_settings.insert("ipv6", ipv6);

        let root_path = ObjectPath::try_from("/")
            .map_err(|e| NetworkError::DBusError(format!("Invalid root path: {e}")))?;

        let (_, active_conn_path) = nm
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

        loop {
            if start.elapsed() > Duration::from_secs(timeout_secs) {
                return Err(NetworkError::WifiConnectionTimeout {
                    ssid: request.ssid.clone(),
                    timeout_secs,
                }
                .into());
            }

            let state = self
                .get_property(
                    &active_path_ref,
                    "org.freedesktop.NetworkManager.Connection.Active",
                    "State",
                )
                .await
                .ok()
                .and_then(|v| v.downcast_ref::<u32>().ok())
                .unwrap_or(0);

            match state {
                // NM_ACTIVE_CONNECTION_STATE_ACTIVATED = 2
                2 => {
                    info!(ssid = %request.ssid, "Wi-Fi connection established");
                    break;
                }
                // NM_ACTIVE_CONNECTION_STATE_DEACTIVATED = 4
                4 => {
                    return Err(NetworkError::WifiError(format!(
                        "Connection to '{}' was rejected or failed",
                        request.ssid
                    ))
                    .into());
                }
                _ => {
                    sleep(Duration::from_millis(500)).await;
                }
            }
        }

        // Return updated status.
        self.wifi_sta_status(request.interface_name.as_deref())
            .await
    }

    async fn disconnect_wifi(&self, interface_name: Option<&str>) -> NGResult<()> {
        info!("Disconnecting Wi-Fi STA");

        let dev_path = self.find_wireless_device(interface_name).await?;
        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        let dev_props = self
            .get_all_properties(&dev_path_ref, "org.freedesktop.NetworkManager.Device")
            .await?;

        let active_conn = prop_object_path(&dev_props, "ActiveConnection").filter(|p| p != "/");

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
                return Ok(WifiStaStatus {
                    connected: false,
                    interface_name: None,
                    ssid: None,
                    bssid: None,
                    security: None,
                    band: None,
                    channel: None,
                    frequency: None,
                    signal_dbm: None,
                    signal_quality: None,
                    ip_address: None,
                    gateway: None,
                    dns: Vec::new(),
                    speed_mbps: None,
                    connected_secs: None,
                });
            }
        };

        let dev_path_ref = ObjectPath::try_from(dev_path.as_str())
            .map_err(|e| NetworkError::DBusError(format!("Invalid device path: {e}")))?;

        let dev_props = self
            .get_all_properties(&dev_path_ref, "org.freedesktop.NetworkManager.Device")
            .await?;

        let iface_name = prop_str(&dev_props, "Interface");
        let state = prop_u32(&dev_props, "State").unwrap_or(0);
        let speed = prop_u32(&dev_props, "Speed");

        if state != NM_DEVICE_STATE_ACTIVATED {
            return Ok(WifiStaStatus {
                connected: false,
                interface_name: iface_name,
                ssid: None,
                bssid: None,
                security: None,
                band: None,
                channel: None,
                frequency: None,
                signal_dbm: None,
                signal_quality: None,
                ip_address: None,
                gateway: None,
                dns: Vec::new(),
                speed_mbps: speed,
                connected_secs: None,
            });
        }

        // Read active AP properties.
        let wifi_props = self
            .get_all_properties(
                &dev_path_ref,
                "org.freedesktop.NetworkManager.Device.Wireless",
            )
            .await
            .unwrap_or_default();

        let (ssid, bssid, security, band, channel, frequency, signal_dbm, signal_quality) =
            if let Some(ap_path) =
                prop_object_path(&wifi_props, "ActiveAccessPoint").filter(|p| p != "/")
            {
                let ap_path_ref = ObjectPath::try_from(ap_path.as_str())
                    .map_err(|e| NetworkError::DBusError(format!("Invalid AP path: {e}")))?;
                let ap_props = self
                    .get_all_properties(&ap_path_ref, "org.freedesktop.NetworkManager.AccessPoint")
                    .await
                    .unwrap_or_default();

                let ssid = prop_byte_array(&ap_props, "Ssid")
                    .map(|b| String::from_utf8_lossy(&b).to_string());
                let bssid = prop_str(&ap_props, "HwAddress");
                let strength = prop_u8(&ap_props, "Strength").unwrap_or(0);
                let freq = prop_u32(&ap_props, "Frequency").unwrap_or(0);
                let flags = prop_u32(&ap_props, "Flags").unwrap_or(0);
                let wpa_flags = prop_u32(&ap_props, "WpaFlags").unwrap_or(0);
                let rsn_flags = prop_u32(&ap_props, "RsnFlags").unwrap_or(0);

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
        use crate::network::ap_manager::ApServiceManager;

        // Resolve ap_mode from cached capabilities.
        let caps = self.detect_capabilities().await.ok();
        let ap_mode = caps
            .as_ref()
            .map(|c| c.ap_mode)
            .unwrap_or(ApMode::Unavailable);
        let sta_will_disconnect = ap_mode == ApMode::Exclusive;

        let mgr = ApServiceManager::from_connection(self.dbus_conn.clone());
        let svc_status =
            mgr.status()
                .await
                .unwrap_or(crate::network::ap_manager::ApServiceStatus {
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
            });
        }

        // Read hostapd.conf for SSID / channel / hw_mode.
        let conf_path = format!(
            "{}/{}",
            crate::network::ap_config::AP_CONFIG_DIR,
            crate::network::ap_config::HOSTAPD_CONF_FILE
        );
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
        let env_path = format!(
            "{}/{}",
            crate::network::ap_config::AP_CONFIG_DIR,
            crate::network::ap_config::AP_ENV_FILE
        );
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
            security: Some(WifiSecurity::Wpa2Psk),
            connected_clients,
            ip_address,
            prefix_length,
            ap_mode,
            sta_will_disconnect,
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

        // In exclusive mode, stash STA connection for restore after stop_ap, then disconnect.
        if caps.ap_mode == ApMode::Exclusive {
            info!("Exclusive mode: stashing STA connection and disconnecting");
            if let Some(stashed) = self.stash_active_sta_connection().await {
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

        // In exclusive mode, restore the previously disconnected STA connection.
        let stashed = self.stashed_sta_for_restore.write().await.take();
        if let Some(s) = stashed {
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
                    sleep(Duration::from_millis(500)).await;
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
            parse_env_value(&current_env, "STA_IFACE").unwrap_or_else(|| "wlan0".to_string());
        let current_uplink_iface = parse_env_value(&current_env, "UPLINK_IFACE")
            .unwrap_or_else(|| current_sta_iface.clone());
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
                let _ = mgr.restart_hostapd().await;
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
                .get_all_properties(&dev_path_ref, "org.freedesktop.NetworkManager.Device")
                .await
                .unwrap_or_default();

            let state = prop_u32(&dev_props, "State").unwrap_or(0);
            if state != NM_DEVICE_STATE_ACTIVATED {
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
                .get_all_properties(&dev_path_ref, "org.freedesktop.NetworkManager.Device")
                .await
                .unwrap_or_default();

            let device_type = prop_u32(&dev_props, "DeviceType").unwrap_or(0);
            let state = prop_u32(&dev_props, "State").unwrap_or(0);
            let iface_name = prop_str(&dev_props, "Interface").unwrap_or_default();

            if device_type != NM_DEVICE_TYPE_ETHERNET || state != NM_DEVICE_STATE_ACTIVATED {
                continue;
            }

            // Re-apply interface config with the new DNS servers.
            let dns_list: Vec<IpAddr> = config
                .servers
                .iter()
                .filter_map(|s| s.to_string().parse::<IpAddr>().ok())
                .collect();

            let apply_config = ConfigureInterfaceRequest {
                method: IpMethod::Dhcp,
                ip_address: None,
                prefix_length: None,
                gateway: None,
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
fn nm_device_type_to_kind(device_type: u32) -> InterfaceKind {
    match device_type {
        NM_DEVICE_TYPE_ETHERNET => InterfaceKind::Ethernet,
        NM_DEVICE_TYPE_WIFI => InterfaceKind::Wifi,
        NM_DEVICE_TYPE_BRIDGE => InterfaceKind::Bridge,
        NM_DEVICE_TYPE_VLAN => InterfaceKind::Vlan,
        14 => InterfaceKind::Loopback, // NM_DEVICE_TYPE_GENERIC (lo)
        _ => InterfaceKind::Unknown,
    }
}

/// Map NM device state to our LinkState.
fn nm_state_to_link_state(state: u32) -> LinkState {
    match state {
        NM_DEVICE_STATE_ACTIVATED => LinkState::Up,
        NM_DEVICE_STATE_DISCONNECTED => LinkState::Down,
        NM_DEVICE_STATE_UNAVAILABLE => LinkState::Down,
        NM_DEVICE_STATE_UNMANAGED => LinkState::Down,
        40..=99 => LinkState::Dormant, // Connecting states
        _ => LinkState::Unknown,
    }
}

/// Map NM Wi-Fi mode constant to our WifiMode.
fn nm_wifi_mode_to_mode(mode: u32) -> WifiMode {
    match mode {
        2 => WifiMode::Station, // NM_802_11_MODE_INFRA
        3 => WifiMode::Ap,      // NM_802_11_MODE_AP
        1 => WifiMode::AdHoc,   // NM_802_11_MODE_ADHOC
        _ => WifiMode::Unknown,
    }
}

/// Derive WifiSecurity from NM AP flags.
fn derive_security(flags: u32, wpa_flags: u32, rsn_flags: u32) -> WifiSecurity {
    const NM_AP_SEC_KEY_MGMT_PSK: u32 = 0x100;
    const NM_AP_SEC_KEY_MGMT_SAE: u32 = 0x400;
    const NM_AP_SEC_KEY_MGMT_802_1X: u32 = 0x200;

    if rsn_flags & NM_AP_SEC_KEY_MGMT_SAE != 0 {
        return WifiSecurity::Wpa3Sae;
    }
    if rsn_flags & NM_AP_SEC_KEY_MGMT_802_1X != 0 {
        return WifiSecurity::Wpa2Enterprise;
    }
    if rsn_flags & NM_AP_SEC_KEY_MGMT_PSK != 0 {
        return WifiSecurity::Wpa2Psk;
    }
    if wpa_flags & NM_AP_SEC_KEY_MGMT_802_1X != 0 {
        return WifiSecurity::WpaEnterprise;
    }
    if wpa_flags & NM_AP_SEC_KEY_MGMT_PSK != 0 {
        return WifiSecurity::WpaPsk;
    }
    if flags & 0x1 != 0 {
        // NM_802_11_AP_FLAGS_PRIVACY — WEP or similar
        return WifiSecurity::Wep;
    }
    WifiSecurity::Open
}

/// Convert frequency (MHz) to channel number.
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
fn frequency_to_band(freq: u32) -> WifiBand {
    match freq {
        2400..=2500 => WifiBand::Band2_4Ghz,
        5150..=5900 => WifiBand::Band5Ghz,
        5925..=7125 => WifiBand::Band6Ghz,
        _ => WifiBand::Unknown,
    }
}

/// Approximate RSSI (dBm) from NM signal quality (0-100).
fn quality_to_rssi(quality: u8) -> i32 {
    -90 + (quality as i32 * 60 / 100)
}

// ─── D-Bus Property Extraction Helpers ───

/// Look up a key in a zvariant Dict by iterating entries.
///
/// `Dict::get` has complex lifetime constraints that make it hard to call
/// with string literal keys. Iteration is O(n) but NM dicts are tiny.
fn dict_lookup_str(dict: &zbus::zvariant::Dict<'_, '_>, key: &str) -> Option<String> {
    dict.iter()
        .find(|(k, _)| k.downcast_ref::<&str>().ok() == Some(key))
        .and_then(|(_, v)| v.downcast_ref::<&str>().ok().map(|s| s.to_string()))
}

fn dict_lookup_u32(dict: &zbus::zvariant::Dict<'_, '_>, key: &str) -> Option<u32> {
    dict.iter()
        .find(|(k, _)| k.downcast_ref::<&str>().ok() == Some(key))
        .and_then(|(_, v)| v.downcast_ref::<u32>().ok())
}

fn prop_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| v.downcast_ref::<&str>().ok().map(|s| s.to_string()))
}

fn prop_u32(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    props.get(key).and_then(|v| v.downcast_ref::<u32>().ok())
}

fn prop_u8(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    props.get(key).and_then(|v| v.downcast_ref::<u8>().ok())
}

fn prop_byte_array(props: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<u8>> {
    props.get(key).and_then(|v| {
        // NM Ssid is `ay` (array of bytes).
        if let Ok(arr) = v.downcast_ref::<&zbus::zvariant::Array>() {
            let bytes: Vec<u8> = arr
                .iter()
                .filter_map(|item| item.downcast_ref::<u8>().ok())
                .collect();
            if bytes.is_empty() {
                None
            } else {
                Some(bytes)
            }
        } else {
            None
        }
    })
}

fn prop_object_path(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props.get(key).and_then(|v| {
        v.downcast_ref::<&ObjectPath<'_>>()
            .ok()
            .map(|p| p.to_string())
    })
}

/// Parse NM IP4Config AddressData property.
///
/// NM >= 1.0 exposes `AddressData` as `aa{sv}` with "address" (string) and "prefix" (uint32).
fn parse_nm_ip4_addresses(props: &HashMap<String, OwnedValue>) -> Vec<Ipv4AddressInfo> {
    let mut result = Vec::new();

    if let Some(addr_data) = props.get("AddressData") {
        if let Ok(arr) = addr_data.downcast_ref::<&zbus::zvariant::Array>() {
            for item in arr.iter() {
                if let Ok(dict) = item.downcast_ref::<&zbus::zvariant::Dict>() {
                    let address =
                        dict_lookup_str(dict, "address").and_then(|s| s.parse::<IpAddr>().ok());
                    let prefix = dict_lookup_u32(dict, "prefix").unwrap_or(24) as u8;

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
fn parse_nm_ip4_nameservers(props: &HashMap<String, OwnedValue>) -> Vec<IpAddr> {
    let mut result = Vec::new();

    if let Some(ns_data) = props.get("NameserverData") {
        if let Ok(arr) = ns_data.downcast_ref::<&zbus::zvariant::Array>() {
            for item in arr.iter() {
                if let Ok(dict) = item.downcast_ref::<&zbus::zvariant::Dict>() {
                    if let Some(addr) =
                        dict_lookup_str(dict, "address").and_then(|s| s.parse::<IpAddr>().ok())
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
fn parse_nm_ip6_addresses(props: &HashMap<String, OwnedValue>) -> Vec<Ipv6AddressInfo> {
    let mut result = Vec::new();

    if let Some(addr_data) = props.get("AddressData") {
        if let Ok(arr) = addr_data.downcast_ref::<&zbus::zvariant::Array>() {
            for item in arr.iter() {
                if let Ok(dict) = item.downcast_ref::<&zbus::zvariant::Dict>() {
                    let address =
                        dict_lookup_str(dict, "address").and_then(|s| s.parse::<IpAddr>().ok());
                    let prefix = dict_lookup_u32(dict, "prefix").unwrap_or(64) as u8;

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
fn parse_nm_ip6_nameservers(props: &HashMap<String, OwnedValue>) -> Vec<IpAddr> {
    // Try NameserverData first (NM >= 1.14), fallback to Nameservers.
    parse_nm_ip4_nameservers(props) // Same format
}

/// Read Rx/Tx byte counters from `/sys/class/net/<iface>/statistics/`.
fn read_sysfs_traffic(iface: &str) -> (Option<u64>, Option<u64>) {
    let rx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/rx_bytes"))
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let tx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/tx_bytes"))
        .ok()
        .and_then(|s| s.trim().parse().ok());
    (rx, tx)
}

/// Read packet counters from `/sys/class/net/<iface>/statistics/`.
fn read_sysfs_counters(iface: &str) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    let read = |stat: &str| -> Option<u64> {
        std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/{stat}"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    };
    (
        read("rx_packets"),
        read("tx_packets"),
        read("rx_errors"),
        read("tx_errors"),
    )
}

// ─── Configuration File Parsing Helpers ───

/// Parse a `key=value` line from an INI-style config file (hostapd.conf).
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

/// Count connected clients via hostapd's control interface.
///
/// Sends the `STA-FIRST` / `STA-NEXT` commands to the UNIX socket at
/// `/var/run/hostapd/<iface>`. Returns `None` if the socket is unavailable.
async fn count_hostapd_clients(iface: &str) -> Option<u32> {
    use tokio::net::UnixDatagram;

    let ctrl_path = format!("/var/run/hostapd/{iface}");
    if !std::path::Path::new(&ctrl_path).exists() {
        return None;
    }

    let client_path = format!("/tmp/ng-gw-hapd-{}-{}", iface, std::process::id());
    let _ = std::fs::remove_file(&client_path);

    let sock = UnixDatagram::bind(&client_path).ok()?;
    sock.connect(&ctrl_path).ok()?;

    // "STA-FIRST" returns the first associated station's MAC, or "FAIL" / empty.
    sock.send(b"STA-FIRST").await.ok()?;

    let mut buf = [0u8; 4096];
    let mut count: u32 = 0;

    loop {
        let n = tokio::time::timeout(Duration::from_millis(200), sock.recv(&mut buf))
            .await
            .ok()?
            .ok()?;

        let resp = std::str::from_utf8(&buf[..n]).unwrap_or("");
        if resp.is_empty() || resp.starts_with("FAIL") {
            break;
        }

        count += 1;

        // Extract MAC from first line for STA-NEXT.
        let mac = resp.lines().next().unwrap_or("");
        if mac.is_empty() || !mac.contains(':') {
            break;
        }

        let next_cmd = format!("STA-NEXT {mac}");
        if sock.send(next_cmd.as_bytes()).await.is_err() {
            break;
        }
    }

    let _ = std::fs::remove_file(&client_path);
    Some(count)
}
