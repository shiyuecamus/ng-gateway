//! AP service manager — controls hostapd / dnsmasq / ap-setup via systemd D-Bus.
//!
//! # Design
//! The three AP-related systemd units are:
//! - `ng-gateway-ap-setup.service` (oneshot) — creates virtual iface, assigns IP, enables NAT
//! - `ng-gateway-hostapd.service` (simple) — runs hostapd
//! - `ng-gateway-dnsmasq.service` (simple) — runs dnsmasq for AP clients
//!
//! This manager provides async helpers to start/stop/restart/query these services
//! via the systemd1 D-Bus interface, keeping the gateway process decoupled from
//! the AP lifecycle.

use ng_gateway_error::{network::NetworkError, NGResult};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info, warn};
use zbus::{proxy, zvariant::OwnedObjectPath, Connection};

/// systemd unit names for the AP stack.
pub const AP_SETUP_UNIT: &str = "ng-gateway-ap-setup.service";
pub const HOSTAPD_UNIT: &str = "ng-gateway-hostapd.service";
pub const DNSMASQ_UNIT: &str = "ng-gateway-dnsmasq.service";

/// All AP units in dependency order.
pub const AP_UNITS: &[&str] = &[AP_SETUP_UNIT, HOSTAPD_UNIT, DNSMASQ_UNIT];

/// Proxy for systemd1 Manager interface.
#[proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    /// Start a unit. `mode` is typically "replace" or "fail".
    #[zbus(name = "StartUnit")]
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;

    /// Stop a unit.
    #[zbus(name = "StopUnit")]
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;

    /// Restart a unit.
    #[zbus(name = "RestartUnit")]
    fn restart_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;

    /// Reload systemd daemon (equivalent to `systemctl daemon-reload`).
    #[zbus(name = "Reload")]
    fn reload(&self) -> zbus::Result<()>;

    /// Get the object path for a unit.
    #[zbus(name = "GetUnit")]
    fn get_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
}

/// Proxy for reading systemd unit properties.
#[proxy(
    interface = "org.freedesktop.DBus.Properties",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdProperties {
    fn get(
        &self,
        interface_name: &str,
        property_name: &str,
    ) -> zbus::Result<zbus::zvariant::OwnedValue>;
}

/// Manages the AP systemd service stack.
pub struct ApServiceManager {
    dbus_conn: Connection,
}

/// Combined status of the AP service stack.
#[derive(Debug, Clone)]
pub struct ApServiceStatus {
    pub setup_active: bool,
    pub hostapd_active: bool,
    pub dnsmasq_active: bool,
}

impl ApServiceStatus {
    /// All three services are active.
    pub fn all_active(&self) -> bool {
        self.setup_active && self.hostapd_active && self.dnsmasq_active
    }

    /// At least hostapd is running (AP is broadcasting).
    pub fn ap_broadcasting(&self) -> bool {
        self.hostapd_active
    }
}

impl ApServiceManager {
    /// Create a new manager, connecting to the system D-Bus.
    pub async fn new() -> NGResult<Self> {
        let dbus_conn = Connection::system().await.map_err(|e| {
            NetworkError::DBusError(format!("Failed to connect to system D-Bus: {e}"))
        })?;
        Ok(Self { dbus_conn })
    }

    /// Create from an existing D-Bus connection (shares the connection with NM manager).
    pub fn from_connection(conn: Connection) -> Self {
        Self { dbus_conn: conn }
    }

    async fn systemd_proxy(&self) -> NGResult<SystemdManagerProxy<'_>> {
        SystemdManagerProxy::new(&self.dbus_conn)
            .await
            .map_err(|e| {
                NetworkError::DBusError(format!("Failed to create systemd proxy: {e}")).into()
            })
    }

    /// Get the ActiveState of a systemd unit.
    async fn unit_active_state(&self, unit_name: &str) -> NGResult<String> {
        let sd = self.systemd_proxy().await?;
        let unit_path = sd
            .get_unit(unit_name)
            .await
            .map_err(|e| NetworkError::ApError(format!("Unit {unit_name} not found: {e}")))?;

        let props_proxy = SystemdPropertiesProxy::builder(&self.dbus_conn)
            .path(unit_path.as_ref())
            .map_err(|e| NetworkError::DBusError(format!("Invalid unit path: {e}")))?
            .build()
            .await
            .map_err(|e| NetworkError::DBusError(format!("Props proxy failed: {e}")))?;

        let val = props_proxy
            .get("org.freedesktop.systemd1.Unit", "ActiveState")
            .await
            .map_err(|e| NetworkError::ApError(format!("Failed to read ActiveState: {e}")))?;

        val.downcast_ref::<&str>()
            .map(|s| s.to_string())
            .map_err(|_| NetworkError::ApError("ActiveState is not a string".to_string()).into())
    }

    /// Check combined status of all AP units.
    pub async fn status(&self) -> NGResult<ApServiceStatus> {
        let setup = self
            .unit_active_state(AP_SETUP_UNIT)
            .await
            .unwrap_or_default();
        let hostapd = self
            .unit_active_state(HOSTAPD_UNIT)
            .await
            .unwrap_or_default();
        let dnsmasq = self
            .unit_active_state(DNSMASQ_UNIT)
            .await
            .unwrap_or_default();

        debug!(setup = %setup, hostapd = %hostapd, dnsmasq = %dnsmasq, "AP service status");

        Ok(ApServiceStatus {
            // oneshot is "inactive" after successful run, so check for "active" or completed state
            setup_active: setup == "active" || setup == "inactive",
            hostapd_active: hostapd == "active",
            dnsmasq_active: dnsmasq == "active",
        })
    }

    /// Start all AP services in order.
    pub async fn start_all(&self) -> NGResult<()> {
        info!("Starting AP service stack...");
        let sd = self.systemd_proxy().await?;

        for unit in AP_UNITS {
            sd.start_unit(unit, "replace")
                .await
                .map_err(|e| NetworkError::ApError(format!("Failed to start {unit}: {e}")))?;
            debug!(unit = unit, "Started");
        }

        // Wait for hostapd to become active.
        self.wait_unit_active(HOSTAPD_UNIT, Duration::from_secs(10))
            .await?;

        info!("AP service stack started");
        Ok(())
    }

    /// Stop all AP services in reverse order.
    pub async fn stop_all(&self) -> NGResult<()> {
        info!("Stopping AP service stack...");
        let sd = self.systemd_proxy().await?;

        for unit in AP_UNITS.iter().rev() {
            if let Err(e) = sd.stop_unit(unit, "replace").await {
                warn!(unit = unit, error = %e, "Failed to stop unit (may already be stopped)");
            }
        }

        info!("AP service stack stopped");
        Ok(())
    }

    /// Restart all AP services (stop → start).
    pub async fn restart_all(&self) -> NGResult<()> {
        self.stop_all().await?;
        sleep(Duration::from_millis(500)).await;
        self.start_all().await
    }

    /// Restart only hostapd (for config changes that don't affect the interface setup).
    pub async fn restart_hostapd(&self) -> NGResult<()> {
        info!("Restarting hostapd...");
        let sd = self.systemd_proxy().await?;
        sd.restart_unit(HOSTAPD_UNIT, "replace")
            .await
            .map_err(|e| NetworkError::ApError(format!("Failed to restart hostapd: {e}")))?;
        self.wait_unit_active(HOSTAPD_UNIT, Duration::from_secs(10))
            .await?;
        info!("hostapd restarted");
        Ok(())
    }

    /// Wait for a unit to reach "active" state, with timeout.
    async fn wait_unit_active(&self, unit_name: &str, timeout: Duration) -> NGResult<()> {
        let start = std::time::Instant::now();
        loop {
            let state = self.unit_active_state(unit_name).await.unwrap_or_default();
            if state == "active" {
                return Ok(());
            }
            if state == "failed" {
                return Err(NetworkError::ApError(format!(
                    "Unit {unit_name} entered 'failed' state"
                ))
                .into());
            }
            if start.elapsed() > timeout {
                return Err(NetworkError::Timeout(format!(
                    "Timed out waiting for {unit_name} to become active (last state: {state})"
                ))
                .into());
            }
            sleep(Duration::from_millis(300)).await;
        }
    }

    /// Trigger systemd daemon-reload (needed after writing new unit files).
    pub async fn daemon_reload(&self) -> NGResult<()> {
        let sd = self.systemd_proxy().await?;
        sd.reload()
            .await
            .map_err(|e| NetworkError::DBusError(format!("systemd daemon-reload failed: {e}")))?;
        Ok(())
    }
}
