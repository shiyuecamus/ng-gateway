//! Wireless hardware capability detection.
//!
//! Parses `iw phy <phy> info` output to determine:
//! - Supported interface modes (managed, AP, monitor, etc.)
//! - STA + AP concurrency (valid interface combinations)
//! - Supported frequency bands (2.4 GHz / 5 GHz / 6 GHz)
//!
//! The `parse_*` functions are pure and cross-platform (useful for testing).
//! The `detect_phy_capabilities`, `resolve_phy_name`, and `probe_virtual_ap`
//! functions invoke `iw` and are Linux-only.
//!
//! On non-Linux hosts, the parse helpers are only exercised by `#[cfg(test)]`.

use ng_gateway_models::domain::prelude::{
    ApMode, StaApCapability, WifiBand, WirelessInterfaceCapability,
};
#[cfg(target_os = "linux")]
use tokio::process::Command;
use tracing::{debug, warn};

/// Fixed tokens in `iw phy <phy> info` output used for parsing.
///
/// Used by both the `parse_*` functions (cross-platform, used in tests) and
/// the `#[cfg(target_os = "linux")]` async functions. Allow dead code to
/// suppress warnings on non-Linux hosts where the async callers are compiled out.
#[allow(dead_code)]
mod iw_tokens {
    pub const SUPPORTED_MODES_HEADER: &str = "Supported interface modes:";
    pub const VALID_COMBOS_HEADER: &str = "valid interface combinations:";
    pub const COMBOS_NOT_SUPPORTED: &str = "interface combinations are not supported";
    pub const MODE_AP: &str = "AP";
    pub const MODE_MANAGED: &str = "managed";
    pub const BULLET_PREFIX: &str = "* ";

    /// sysfs path template for resolving the phy name of a wireless interface.
    pub const PHY80211_NAME_FMT: &str = "/sys/class/net/{iface}/phy80211/name";
    pub const BAND_PREFIX: &str = "Band ";
}

/// Detect wireless capabilities for a given phy by running `iw phy <phy> info`.
///
/// When `iw phy info` reports AP in supported modes but `interface combinations
/// are not supported` (common with Realtek drivers), we probe concurrent
/// capability by creating a temporary virtual AP interface. This is the only
/// reliable way to detect STA+AP concurrency for these drivers.
///
/// Returns `None` if the command fails or `iw` is not installed.
#[cfg(target_os = "linux")]
pub async fn detect_phy_capabilities(
    iface_name: &str,
    phy_name: &str,
) -> Option<WirelessInterfaceCapability> {
    let output = Command::new("iw")
        .args(["phy", phy_name, "info"])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        warn!(phy = phy_name, "iw phy info failed");
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut cap = parse_phy_info(iface_name, phy_name, &stdout);

    if !cap.supports_sta_ap_concurrent && has_ap_mode(&cap) && has_no_combinations_section(&stdout)
    {
        debug!(
            phy = phy_name,
            iface = iface_name,
            "No interface combinations advertised — probing virtual AP support"
        );
        cap.supports_sta_ap_concurrent = probe_virtual_ap(iface_name).await;
        if cap.supports_sta_ap_concurrent {
            debug!(
                phy = phy_name,
                "Virtual AP probe succeeded — concurrent STA+AP confirmed"
            );
        } else {
            debug!(
                phy = phy_name,
                "Virtual AP probe failed — exclusive mode only"
            );
        }
    }

    Some(cap)
}

/// Check if the iw output explicitly lacks a `valid interface combinations` section.
fn has_no_combinations_section(iw_output: &str) -> bool {
    for line in iw_output.lines() {
        let trimmed = line.trim();
        if trimmed == iw_tokens::COMBOS_NOT_SUPPORTED {
            return true;
        }
        if trimmed.starts_with(iw_tokens::VALID_COMBOS_HEADER) {
            return false;
        }
    }
    true
}

/// Check if the capability includes AP mode in supported modes.
fn has_ap_mode(cap: &WirelessInterfaceCapability) -> bool {
    cap.supported_modes
        .iter()
        .any(|m| m.eq_ignore_ascii_case(iw_tokens::MODE_AP))
}

/// Probe concurrent STA+AP by creating a virtual AP interface, verifying it
/// can actually be brought up, then tearing it down.
///
/// Some drivers (notably Realtek RTL8852BE / rtw89) allow *creating* a virtual
/// AP interface (`iw dev ... interface add ... type __ap`) but refuse to bring
/// it up (`ip link set ... up` → `EBUSY`).  Only testing both operations gives
/// a reliable answer.
#[cfg(target_os = "linux")]
async fn probe_virtual_ap(sta_iface: &str) -> bool {
    let probe_name = format!("{sta_iface}_probe");

    let create = Command::new("iw")
        .args([
            "dev",
            sta_iface,
            "interface",
            "add",
            &probe_name,
            "type",
            "__ap",
        ])
        .output()
        .await;

    let created = matches!(&create, Ok(out) if out.status.success());
    if !created {
        return false;
    }

    // The interface was created — now verify it can actually be activated.
    // Drivers that fake concurrency support will fail here with EBUSY.
    let up = Command::new("ip")
        .args(["link", "set", &probe_name, "up"])
        .output()
        .await;
    let can_activate = matches!(&up, Ok(out) if out.status.success());

    if !can_activate {
        debug!(
            iface = probe_name,
            "Virtual AP created but cannot be brought up — driver does not truly support concurrency"
        );
    }

    // Always clean up: down + delete.
    let _ = Command::new("ip")
        .args(["link", "set", &probe_name, "down"])
        .output()
        .await;
    let _ = Command::new("iw")
        .args(["dev", &probe_name, "del"])
        .output()
        .await;

    can_activate
}

/// Resolve the phy name for a wireless interface via `/sys/class/net/<iface>/phy80211/name`.
#[inline]
#[cfg(target_os = "linux")]
pub async fn resolve_phy_name(iface_name: &str) -> Option<String> {
    let path = iw_tokens::PHY80211_NAME_FMT.replace("{iface}", iface_name);
    tokio::fs::read_to_string(&path)
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse the full output of `iw phy <phy> info`.
#[inline]
fn parse_phy_info(iface_name: &str, phy_name: &str, output: &str) -> WirelessInterfaceCapability {
    let supported_modes = parse_supported_modes(output);
    let supports_sta_ap = parse_sta_ap_concurrent(output);
    let supported_bands = parse_supported_bands(output);

    debug!(
        phy = phy_name,
        iface = iface_name,
        modes = ?supported_modes,
        sta_ap = supports_sta_ap,
        bands = ?supported_bands,
        "Parsed wireless capabilities"
    );

    WirelessInterfaceCapability {
        name: iface_name.to_string(),
        phy: phy_name.to_string(),
        supported_modes,
        supports_sta_ap_concurrent: supports_sta_ap,
        supported_bands,
        current_mode: None,
    }
}

/// Extract supported interface modes from `iw phy info`.
///
/// Looks for the section:
/// ```text
///     Supported interface modes:
///          * IBSS
///          * managed
///          * AP
///          * monitor
/// ```
#[inline]
fn parse_supported_modes(output: &str) -> Vec<String> {
    let mut modes = Vec::new();
    let mut in_section = false;

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with(iw_tokens::SUPPORTED_MODES_HEADER) {
            in_section = true;
            continue;
        }

        if in_section {
            if let Some(mode) = trimmed.strip_prefix(iw_tokens::BULLET_PREFIX) {
                modes.push(mode.trim().to_string());
            } else {
                break;
            }
        }
    }

    modes
}

/// Check whether the phy supports simultaneous STA + AP.
///
/// Two detection methods, tried in order:
///
/// 1. **`valid interface combinations` section** — if present and contains both
///    `managed` and `AP` in the same combination block, returns `true`.
///
/// 2. **`interface combinations are not supported`** — some drivers (notably
///    Realtek RTL8852BE on Orange Pi 5 Plus) omit this section entirely but
///    *do* support creating virtual AP interfaces via `iw dev add`. For these
///    drivers, we flag the result as `Unknown` (neither confirmed nor denied).
///    The runtime probe (creating a temporary virtual interface) happens at a
///    higher level (`detect_phy_capabilities` or the shell `init-network.sh`).
///
/// Expected pattern for method 1:
/// ```text
///     valid interface combinations:
///          * #{ managed } <= 1, #{ AP } <= 1,
///            total <= 2, #channels <= 1
/// ```
#[inline]
fn parse_sta_ap_concurrent(output: &str) -> bool {
    let mut in_section = false;
    let mut combo_block = String::new();
    let mut has_combinations_section = false;

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed == iw_tokens::COMBOS_NOT_SUPPORTED {
            return false;
        }

        if trimmed.starts_with(iw_tokens::VALID_COMBOS_HEADER) {
            in_section = true;
            has_combinations_section = true;
            continue;
        }

        if in_section {
            if trimmed.starts_with('*') || trimmed.starts_with('#') || trimmed.starts_with("total")
            {
                combo_block.push(' ');
                combo_block.push_str(trimmed);
            } else if !trimmed.is_empty() && !trimmed.starts_with(',') {
                if check_combo_block(&combo_block) {
                    return true;
                }
                if !trimmed.contains("<=") && !trimmed.contains('{') {
                    break;
                }
                combo_block.clear();
                combo_block.push_str(trimmed);
            } else {
                combo_block.push(' ');
                combo_block.push_str(trimmed);
            }
        }
    }

    if has_combinations_section {
        return check_combo_block(&combo_block);
    }

    // No combinations section at all — cannot confirm concurrent support
    // from iw output alone; higher layers should probe.
    false
}

/// Check whether a single combination block contains both `managed` and `AP`.
#[inline]
fn check_combo_block(block: &str) -> bool {
    let lower = block.to_lowercase();
    lower.contains(iw_tokens::MODE_MANAGED) && lower.contains(&iw_tokens::MODE_AP.to_lowercase())
}

/// Parse supported frequency bands from `iw phy info`.
///
/// Looks for `Band N:` sections and checks frequency ranges.
#[inline]
fn parse_supported_bands(output: &str) -> Vec<WifiBand> {
    let mut bands = Vec::new();
    let mut has_2g = false;
    let mut has_5g = false;
    let mut has_6g = false;

    for line in output.lines() {
        let trimmed = line.trim();

        // Look for frequency lines like: "* 2412.0 MHz [1]" or "* 5180 MHz [36]"
        if trimmed.starts_with(iw_tokens::BULLET_PREFIX) && trimmed.contains("MHz") {
            if let Some(freq) = extract_frequency(trimmed) {
                match freq {
                    2400..=2500 => has_2g = true,
                    5150..=5900 => has_5g = true,
                    5925..=7125 => has_6g = true,
                    _ => {}
                }
            }
        }
    }

    if has_2g {
        bands.push(WifiBand::Band2_4Ghz);
    }
    if has_5g {
        bands.push(WifiBand::Band5Ghz);
    }
    if has_6g {
        bands.push(WifiBand::Band6Ghz);
    }

    bands
}

/// Extract frequency in MHz from a line like `* 2412.0 MHz [1] (20.0 dBm)`.
#[inline]
fn extract_frequency(line: &str) -> Option<u32> {
    let after_star = line.strip_prefix(iw_tokens::BULLET_PREFIX)?.trim();
    let mhz_part = after_star.split_whitespace().next()?;
    // Handle "2412.0" or "2412"
    let int_part = mhz_part.split('.').next()?;
    int_part.parse::<u32>().ok()
}

/// Aggregate per-interface capabilities into a single `StaApCapability` for the platform.
///
/// Some drivers (especially Realtek) report AP in `Supported interface modes` but omit
/// `valid interface combinations`, causing `supports_sta_ap_concurrent` to be false.
/// We treat those as `Unknown` rather than `NotSupported` so that the AP management
/// UI remains available — the user can still run AP in exclusive mode.
#[inline]
pub fn aggregate_sta_ap_capability(interfaces: &[WirelessInterfaceCapability]) -> StaApCapability {
    if interfaces.is_empty() {
        return StaApCapability::NotSupported;
    }

    // If any single card supports concurrent STA + AP, that's the best case.
    if interfaces.iter().any(|i| i.supports_sta_ap_concurrent) {
        return StaApCapability::SingleCardConcurrent;
    }

    // If there are 2+ wireless interfaces, dual-card is possible.
    if interfaces.len() >= 2 {
        return StaApCapability::DualCard;
    }

    // Single card with AP in supported modes but no `valid interface combinations`
    // reported — the driver likely supports AP but doesn't advertise concurrency.
    let supports_ap_mode = interfaces.iter().any(iface_supports_ap);
    if supports_ap_mode {
        return StaApCapability::Unknown;
    }

    StaApCapability::NotSupported
}

/// Derive the high-level [`ApMode`] from raw hardware capabilities.
///
/// This translates the low-level `StaApCapability` + per-interface mode info
/// into a single actionable enum that drives both UI behavior and backend logic.
#[inline]
pub fn determine_ap_mode(
    interfaces: &[WirelessInterfaceCapability],
    sta_ap: StaApCapability,
) -> ApMode {
    match sta_ap {
        StaApCapability::SingleCardConcurrent => ApMode::Concurrent,
        StaApCapability::DualCard => ApMode::DedicatedCard,
        StaApCapability::Unknown => {
            if interfaces.iter().any(iface_supports_ap) {
                ApMode::Exclusive
            } else {
                ApMode::Unavailable
            }
        }
        StaApCapability::NotSupported => ApMode::Unavailable,
    }
}

/// Check whether a wireless interface has AP in its supported modes list.
#[inline]
fn iface_supports_ap(iface: &WirelessInterfaceCapability) -> bool {
    iface
        .supported_modes
        .iter()
        .any(|m| m.eq_ignore_ascii_case(iw_tokens::MODE_AP))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_IW_OUTPUT: &str = r#"
Wiphy phy0
    max # scan SSIDs: 4
    Supported interface modes:
         * IBSS
         * managed
         * AP
         * AP/VLAN
         * monitor
    valid interface combinations:
         * #{ managed } <= 1, #{ AP } <= 1,
           total <= 2, #channels <= 1
    Band 1:
        * 2412.0 MHz [1] (20.0 dBm)
        * 2417.0 MHz [2] (20.0 dBm)
        * 2462.0 MHz [11] (20.0 dBm)
    Band 2:
        * 5180.0 MHz [36] (23.0 dBm)
        * 5200.0 MHz [40] (23.0 dBm)
"#;

    #[test]
    fn test_parse_supported_modes() {
        let modes = parse_supported_modes(SAMPLE_IW_OUTPUT);
        assert!(modes.contains(&"managed".to_string()));
        assert!(modes.contains(&"AP".to_string()));
        assert!(modes.contains(&"monitor".to_string()));
    }

    #[test]
    fn test_parse_sta_ap_concurrent() {
        assert!(parse_sta_ap_concurrent(SAMPLE_IW_OUTPUT));
    }

    #[test]
    fn test_parse_supported_bands() {
        let bands = parse_supported_bands(SAMPLE_IW_OUTPUT);
        assert!(bands.contains(&WifiBand::Band2_4Ghz));
        assert!(bands.contains(&WifiBand::Band5Ghz));
        assert!(!bands.contains(&WifiBand::Band6Ghz));
    }

    #[test]
    fn test_no_concurrent() {
        let output = r#"
    valid interface combinations:
         * #{ managed } <= 1,
           total <= 1, #channels <= 1
"#;
        assert!(!parse_sta_ap_concurrent(output));
    }

    #[test]
    fn test_interface_combinations_not_supported() {
        let output = r#"
	Supported interface modes:
		 * managed
		 * AP
		 * AP/VLAN
		 * monitor
		 * P2P-client
		 * P2P-GO
	interface combinations are not supported
"#;
        assert!(!parse_sta_ap_concurrent(output));
        let modes = parse_supported_modes(output);
        assert!(modes.contains(&"AP".to_string()));
        assert!(modes.contains(&"managed".to_string()));
    }

    #[test]
    fn test_no_combinations_section_at_all() {
        let output = r#"
	Supported interface modes:
		 * managed
		 * AP
	Supported commands:
		 * start_ap
"#;
        assert!(!parse_sta_ap_concurrent(output));
    }

    #[test]
    fn test_has_no_combinations_section() {
        let with_explicit = "\tinterface combinations are not supported\n";
        assert!(has_no_combinations_section(with_explicit));

        let with_valid = "\tvalid interface combinations:\n\t\t* #{ managed } <= 1\n";
        assert!(!has_no_combinations_section(with_valid));

        let neither = "\tSupported interface modes:\n\t\t* managed\n";
        assert!(has_no_combinations_section(neither));
    }

    #[test]
    fn test_aggregate_single_card_with_ap_no_combinations() {
        // Realtek-style driver: supports AP mode but no `valid interface combinations`.
        let iface = WirelessInterfaceCapability {
            name: "wlP2p33s0".to_string(),
            phy: "phy0".to_string(),
            supported_modes: vec![
                "managed".to_string(),
                "AP".to_string(),
                "monitor".to_string(),
            ],
            supports_sta_ap_concurrent: false,
            supported_bands: vec![WifiBand::Band2_4Ghz, WifiBand::Band5Ghz],
            current_mode: None,
        };
        assert_eq!(
            aggregate_sta_ap_capability(&[iface]),
            StaApCapability::Unknown,
        );
    }

    #[test]
    fn test_aggregate_single_card_no_ap_mode() {
        // Card that only supports managed mode — truly not AP capable.
        let iface = WirelessInterfaceCapability {
            name: "wlan0".to_string(),
            phy: "phy0".to_string(),
            supported_modes: vec!["managed".to_string()],
            supports_sta_ap_concurrent: false,
            supported_bands: vec![WifiBand::Band2_4Ghz],
            current_mode: None,
        };
        assert_eq!(
            aggregate_sta_ap_capability(&[iface]),
            StaApCapability::NotSupported,
        );
    }

    #[test]
    fn test_determine_ap_mode_concurrent() {
        let iface = WirelessInterfaceCapability {
            name: "wlan0".to_string(),
            phy: "phy0".to_string(),
            supported_modes: vec!["managed".to_string(), "AP".to_string()],
            supports_sta_ap_concurrent: true,
            supported_bands: vec![WifiBand::Band2_4Ghz],
            current_mode: None,
        };
        assert_eq!(
            determine_ap_mode(&[iface], StaApCapability::SingleCardConcurrent),
            ApMode::Concurrent,
        );
    }

    #[test]
    fn test_determine_ap_mode_exclusive() {
        let iface = WirelessInterfaceCapability {
            name: "wlP2p33s0".to_string(),
            phy: "phy0".to_string(),
            supported_modes: vec!["managed".to_string(), "AP".to_string()],
            supports_sta_ap_concurrent: false,
            supported_bands: vec![WifiBand::Band2_4Ghz, WifiBand::Band5Ghz],
            current_mode: None,
        };
        assert_eq!(
            determine_ap_mode(&[iface], StaApCapability::Unknown),
            ApMode::Exclusive,
        );
    }

    #[test]
    fn test_determine_ap_mode_unavailable() {
        assert_eq!(
            determine_ap_mode(&[], StaApCapability::NotSupported),
            ApMode::Unavailable,
        );
    }
}
