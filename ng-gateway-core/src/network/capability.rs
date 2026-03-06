//! Wireless hardware capability detection.
//!
//! Parses `iw phy <phy> info` output to determine:
//! - Supported interface modes (managed, AP, monitor, etc.)
//! - STA + AP concurrency (valid interface combinations)
//! - Supported frequency bands (2.4 GHz / 5 GHz / 6 GHz)
//!
//! This module is used on Linux; other platforms return `Unknown` capabilities.

use ng_gateway_models::domain::prelude::{
    ApMode, StaApCapability, WifiBand, WirelessInterfaceCapability,
};
use tokio::process::Command;
use tracing::{debug, warn};

/// Detect wireless capabilities for a given phy by running `iw phy <phy> info`.
///
/// Returns `None` if the command fails or `iw` is not installed.
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
    Some(parse_phy_info(iface_name, phy_name, &stdout))
}

/// Resolve the phy name for a wireless interface via `/sys/class/net/<iface>/phy80211/name`.
pub async fn resolve_phy_name(iface_name: &str) -> Option<String> {
    let path = format!("/sys/class/net/{iface_name}/phy80211/name");
    tokio::fs::read_to_string(&path)
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse the full output of `iw phy <phy> info`.
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
fn parse_supported_modes(output: &str) -> Vec<String> {
    let mut modes = Vec::new();
    let mut in_section = false;

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("Supported interface modes:") {
            in_section = true;
            continue;
        }

        if in_section {
            if let Some(mode) = trimmed.strip_prefix("* ") {
                modes.push(mode.trim().to_string());
            } else {
                break;
            }
        }
    }

    modes
}

/// Check whether the phy supports simultaneous STA + AP via `valid interface combinations`.
///
/// Expected pattern:
/// ```text
///     valid interface combinations:
///          * #{ managed } <= 1, #{ AP } <= 1,
///            total <= 2, #channels <= 1
/// ```
fn parse_sta_ap_concurrent(output: &str) -> bool {
    let mut in_section = false;
    let mut combo_block = String::new();

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("valid interface combinations:") {
            in_section = true;
            continue;
        }

        if in_section {
            if trimmed.starts_with('*') || trimmed.starts_with('#') || trimmed.starts_with("total")
            {
                combo_block.push(' ');
                combo_block.push_str(trimmed);
            } else if !trimmed.is_empty() && !trimmed.starts_with(',') {
                // Check the accumulated block, then reset for the next combo.
                if check_combo_block(&combo_block) {
                    return true;
                }
                // If the line doesn't look like a continuation, we've left the section.
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

    // Check the last block.
    check_combo_block(&combo_block)
}

/// Check whether a single combination block contains both `managed` and `AP`.
fn check_combo_block(block: &str) -> bool {
    let lower = block.to_lowercase();
    lower.contains("managed") && lower.contains("ap")
}

/// Parse supported frequency bands from `iw phy info`.
///
/// Looks for `Band N:` sections and checks frequency ranges.
fn parse_supported_bands(output: &str) -> Vec<WifiBand> {
    let mut bands = Vec::new();
    let mut has_2g = false;
    let mut has_5g = false;
    let mut has_6g = false;

    for line in output.lines() {
        let trimmed = line.trim();

        // Look for frequency lines like: "* 2412.0 MHz [1]" or "* 5180 MHz [36]"
        if trimmed.starts_with("* ") && trimmed.contains("MHz") {
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
fn extract_frequency(line: &str) -> Option<u32> {
    let after_star = line.strip_prefix("* ")?.trim();
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
    let supports_ap_mode = interfaces.iter().any(|i| {
        i.supported_modes
            .iter()
            .any(|m| m.eq_ignore_ascii_case("ap"))
    });
    if supports_ap_mode {
        return StaApCapability::Unknown;
    }

    StaApCapability::NotSupported
}

/// Derive the high-level [`ApMode`] from raw hardware capabilities.
///
/// This translates the low-level `StaApCapability` + per-interface mode info
/// into a single actionable enum that drives both UI behavior and backend logic.
pub fn determine_ap_mode(
    interfaces: &[WirelessInterfaceCapability],
    sta_ap: StaApCapability,
) -> ApMode {
    match sta_ap {
        StaApCapability::SingleCardConcurrent => ApMode::Concurrent,
        StaApCapability::DualCard => ApMode::DedicatedCard,
        StaApCapability::Unknown => {
            let has_ap_mode = interfaces.iter().any(|i| {
                i.supported_modes
                    .iter()
                    .any(|m| m.eq_ignore_ascii_case("ap"))
            });
            if has_ap_mode {
                ApMode::Exclusive
            } else {
                ApMode::Unavailable
            }
        }
        StaApCapability::NotSupported => ApMode::Unavailable,
    }
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
