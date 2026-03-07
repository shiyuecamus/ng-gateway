//! High-level network service façade.
//!
//! [`NetworkService`] is the entry point consumed by REST API handlers.
//! It delegates to the platform-specific [`PlatformNetworkManager`] and manages
//! caching, capability state, and startup AP initialization.

#[cfg(target_os = "linux")]
use crate::network::ap_manager::ApServiceManager;
use crate::network::platform::{self, PlatformNetworkManager};
use ng_gateway_error::NGResult;
use ng_gateway_models::domain::prelude::{
    ApStatus, ConfigureApRequest, ConfigureDnsRequest, ConfigureInterfaceRequest, DnsConfig,
    InterfaceKind, LinkState, NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary,
    WifiAccessPoint, WifiConnectPreflight, WifiConnectRequest, WifiStaStatus, WiredStatus,
};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, instrument, warn};

/// How long cached capabilities remain valid before requiring a refresh.
const CAPABILITIES_TTL_SECS: u64 = 60;

/// Cached capabilities with timestamp for TTL-based invalidation.
struct CachedCapabilities {
    data: NetworkCapabilities,
    fetched_at: Instant,
}

impl CachedCapabilities {
    fn is_valid(&self) -> bool {
        self.fetched_at.elapsed().as_secs() < CAPABILITIES_TTL_SECS
    }
}

/// High-level network management service.
///
/// Thread-safe (`Arc`-wrapped internally) — clone freely across handlers.
pub struct NetworkService {
    /// Platform-specific network manager.
    manager: Box<dyn PlatformNetworkManager>,
    /// Cached platform capabilities with TTL (refreshed on startup, on demand, or after expiry).
    capabilities: Arc<RwLock<Option<CachedCapabilities>>>,
}

impl NetworkService {
    /// Create and initialize the network service.
    #[instrument(name = "network-service-init")]
    pub async fn new() -> NGResult<Self> {
        info!("Initializing network service...");

        let manager = platform::create_platform_manager().await?;

        let service = Self {
            manager,
            capabilities: Arc::new(RwLock::new(None)),
        };

        // Pre-warm capabilities cache.
        match service.refresh_capabilities().await {
            Ok(caps) => {
                info!(
                    platform = ?caps.platform,
                    nm_available = caps.network_manager_available,
                    can_wifi = caps.can_scan_wifi,
                    can_ap = caps.can_manage_ap,
                    "Platform capabilities detected"
                );
            }
            Err(e) => {
                warn!("Failed to detect platform capabilities on startup: {e}");
            }
        }

        // Check AP service status on Linux (may already be running via boot-time auto-provision).
        #[cfg(target_os = "linux")]
        service.check_ap_state().await;

        info!("Network service initialized");
        Ok(service)
    }

    // ─── Interface Discovery ───

    /// List all network interfaces.
    #[inline]
    pub async fn list_interfaces(&self) -> NGResult<Vec<NetworkInterfaceSummary>> {
        self.manager.list_interfaces().await
    }

    /// Get detailed info for a specific interface.
    #[inline]
    pub async fn get_interface(&self, name: &str) -> NGResult<NetworkInterfaceDetail> {
        self.manager.get_interface(name).await
    }

    // ─── Capabilities ───

    /// Get cached capabilities, refreshing if expired or absent.
    pub async fn capabilities(&self) -> NGResult<NetworkCapabilities> {
        {
            let guard = self.capabilities.read().await;
            if let Some(cached) = guard.as_ref() {
                if cached.is_valid() {
                    return Ok(cached.data.clone());
                }
            }
        }
        self.refresh_capabilities().await
    }

    /// Force-refresh platform capabilities and reset the TTL.
    pub async fn refresh_capabilities(&self) -> NGResult<NetworkCapabilities> {
        let caps = self.manager.detect_capabilities().await?;
        let mut guard = self.capabilities.write().await;
        *guard = Some(CachedCapabilities {
            data: caps.clone(),
            fetched_at: Instant::now(),
        });
        Ok(caps)
    }

    // ─── Aggregated Status ───

    /// Get the best wired interface with enriched status (gateway, DNS filled in).
    ///
    /// Selection priority: connected (up) ethernet > first ethernet > none.
    /// This is the single source of truth for the "Wired Network" tab —
    /// the frontend does not need to do interface selection.
    pub async fn wired_status(&self) -> NGResult<WiredStatus> {
        let all = self.manager.list_interfaces().await?;
        let ethernet: Vec<NetworkInterfaceSummary> = all
            .into_iter()
            .filter(|i| i.kind == InterfaceKind::Ethernet)
            .collect();

        if ethernet.is_empty() {
            return Ok(WiredStatus {
                available: false,
                interface: None,
                all_interfaces: Vec::new(),
            });
        }

        let best = ethernet
            .iter()
            .find(|i| i.link_state == LinkState::Up)
            .or(ethernet.first())
            .cloned();

        Ok(WiredStatus {
            available: true,
            interface: best,
            all_interfaces: ethernet,
        })
    }

    // ─── Interface Configuration ───

    /// Configure IP settings for an interface.
    #[inline]
    pub async fn configure_interface(
        &self,
        name: &str,
        config: &ConfigureInterfaceRequest,
    ) -> NGResult<()> {
        self.manager.configure_interface(name, config).await
    }

    // ─── Wi-Fi ───

    /// Scan for Wi-Fi access points.
    #[inline]
    pub async fn scan_wifi(&self, interface_name: Option<&str>) -> NGResult<Vec<WifiAccessPoint>> {
        self.manager.scan_wifi(interface_name).await
    }

    /// Pre-flight check for Wi-Fi connect — returns side-effect info for the frontend.
    #[inline]
    pub async fn wifi_connect_preflight(
        &self,
        request: &WifiConnectRequest,
    ) -> NGResult<WifiConnectPreflight> {
        self.manager.wifi_connect_preflight(request).await
    }

    /// Connect to a Wi-Fi network.
    ///
    /// If AP is running in exclusive mode, this will orchestrate the full
    /// stop-AP → connect-STA → (optionally restore-AP) sequence.
    #[inline]
    pub async fn connect_wifi(&self, request: &WifiConnectRequest) -> NGResult<WifiStaStatus> {
        self.manager.connect_wifi(request).await
    }

    /// Disconnect Wi-Fi STA.
    #[inline]
    pub async fn disconnect_wifi(&self, interface_name: Option<&str>) -> NGResult<()> {
        self.manager.disconnect_wifi(interface_name).await
    }

    /// Get Wi-Fi STA connection status.
    #[inline]
    pub async fn wifi_sta_status(&self, interface_name: Option<&str>) -> NGResult<WifiStaStatus> {
        self.manager.wifi_sta_status(interface_name).await
    }

    // ─── AP Hotspot ───

    /// Get AP hotspot status.
    #[inline]
    pub async fn ap_status(&self) -> NGResult<ApStatus> {
        self.manager.ap_status().await
    }

    /// Start the AP hotspot.
    #[inline]
    pub async fn start_ap(&self) -> NGResult<ApStatus> {
        self.manager.start_ap().await
    }

    /// Stop the AP hotspot.
    #[inline]
    pub async fn stop_ap(&self) -> NGResult<ApStatus> {
        self.manager.stop_ap().await
    }

    /// Configure AP hotspot.
    #[inline]
    pub async fn configure_ap(&self, config: &ConfigureApRequest) -> NGResult<ApStatus> {
        self.manager.configure_ap(config).await
    }

    // ─── DNS ───

    /// Get current DNS configuration.
    #[inline]
    pub async fn get_dns(&self) -> NGResult<DnsConfig> {
        self.manager.get_dns().await
    }

    /// Set DNS configuration.
    #[inline]
    pub async fn configure_dns(&self, config: &ConfigureDnsRequest) -> NGResult<()> {
        self.manager.configure_dns(config).await
    }

    /// Check AP service status on startup (Linux only).
    ///
    /// The AP hotspot may have been started by `ng-gateway-ap-auto.service`
    /// (boot-time auto-provision for exclusive-mode hardware) before the gateway
    /// process launched.  We log the current state for operational visibility
    /// but do not attempt to start or stop AP — that is handled by the systemd
    /// unit (boot-time) or by the user via the Web UI / API (runtime).
    #[cfg(target_os = "linux")]
    async fn check_ap_state(&self) {
        match ApServiceManager::new().await {
            Ok(mgr) => match mgr.status().await {
                Ok(status) => {
                    if status.ap_broadcasting() {
                        info!(
                            "AP hotspot is active (started by boot-time auto-provision \
                             or previous manual action)"
                        );
                    } else {
                        info!(
                            "AP hotspot is not broadcasting — normal when a WiFi or \
                             wired management channel is available"
                        );
                    }
                }
                Err(e) => {
                    warn!("Failed to query AP service status: {e}");
                }
            },
            Err(e) => {
                info!("AP service manager not available (expected on dev machines): {e}");
            }
        }
    }
}
