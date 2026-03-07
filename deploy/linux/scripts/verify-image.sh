#!/usr/bin/env bash
set -euo pipefail

# verify-image.sh
#
# Post-flash QA verification script for factory-flashed NG Gateway devices.
#
# Usage:
#   sudo bash verify-image.sh [--json] [--golden-machine-id <ID>]
#
# Exit code: 0 = all PASS, 1 = at least one FAIL.

SCRIPT_NAME="$(basename "$0")"
LOG_TAG="[verify]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_common.sh"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

JSON_MODE=0
GOLDEN_MACHINE_ID=""
TOTAL_PASS=0
TOTAL_FAIL=0
TOTAL_WARN=0
RESULTS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --json)               JSON_MODE=1;             shift ;;
    --golden-machine-id)  GOLDEN_MACHINE_ID="$2";  shift 2 ;;
    -h|--help)
      echo "Usage: ${SCRIPT_NAME} [--json] [--golden-machine-id <ID>]"
      exit 0
      ;;
    *) shift ;;
  esac
done

# ─── Check Helpers ───

check_pass() {
  local phase="$1" check="$2" detail="${3:-}"
  TOTAL_PASS=$((TOTAL_PASS + 1))
  RESULTS+=("{\"phase\":\"${phase}\",\"check\":\"${check}\",\"status\":\"PASS\",\"detail\":\"${detail}\"}")
  [[ ${JSON_MODE} -eq 0 ]] && echo -e "  ${GREEN}[PASS]${NC} ${check} ${detail:+— ${detail}}"
}

check_fail() {
  local phase="$1" check="$2" detail="${3:-}"
  TOTAL_FAIL=$((TOTAL_FAIL + 1))
  RESULTS+=("{\"phase\":\"${phase}\",\"check\":\"${check}\",\"status\":\"FAIL\",\"detail\":\"${detail}\"}")
  [[ ${JSON_MODE} -eq 0 ]] && echo -e "  ${RED}[FAIL]${NC} ${check} ${detail:+— ${detail}}"
}

check_warn() {
  local phase="$1" check="$2" detail="${3:-}"
  TOTAL_WARN=$((TOTAL_WARN + 1))
  RESULTS+=("{\"phase\":\"${phase}\",\"check\":\"${check}\",\"status\":\"WARN\",\"detail\":\"${detail}\"}")
  [[ ${JSON_MODE} -eq 0 ]] && echo -e "  ${YELLOW}[WARN]${NC} ${check} ${detail:+— ${detail}}"
}

phase_header() {
  [[ ${JSON_MODE} -eq 0 ]] && echo "" && echo "━━━ Phase $1: $2 ━━━"
}

# ─── Phase 1: Boot Integrity ───

phase_header 1 "Boot Integrity"

if [[ -f /var/lib/ng-gateway/.first-boot-done ]]; then
  fb_time=$(cat /var/lib/ng-gateway/.first-boot-done 2>/dev/null || echo "unknown")
  check_pass "boot" "First-boot completed" "${fb_time}"
else
  check_fail "boot" "First-boot marker missing" "/var/lib/ng-gateway/.first-boot-done not found"
fi

ROOT_DEV=$(findmnt -n -o SOURCE / 2>/dev/null | head -1 || true)
if [[ -n "${ROOT_DEV}" ]]; then
  ROOT_SIZE_KB=$(df -k "${ROOT_DEV}" 2>/dev/null | awk 'NR==2{print $2}')

  if parse_block_device "${ROOT_DEV}"; then
    ROOT_DISK="${_PBD_DISK}"
    if [[ -b "${ROOT_DISK}" ]]; then
      DISK_SIZE_KB=$(($(blockdev --getsize64 "${ROOT_DISK}" 2>/dev/null || echo 0) / 1024))
      if [[ ${DISK_SIZE_KB} -gt 0 ]] && [[ ${ROOT_SIZE_KB} -gt 0 ]]; then
        RATIO=$((ROOT_SIZE_KB * 100 / DISK_SIZE_KB))
        if [[ ${RATIO} -ge 85 ]]; then
          check_pass "boot" "Root partition expanded" "${RATIO}% of disk ($(( ROOT_SIZE_KB / 1048576 )) GB)"
        else
          check_fail "boot" "Root partition too small" "Only ${RATIO}% of disk — first-boot resize may have failed"
        fi
      fi
    fi
  fi
fi

if [[ -s /etc/machine-id ]]; then
  MID=$(cat /etc/machine-id)
  check_pass "boot" "machine-id generated" "${MID}"
else
  check_fail "boot" "machine-id is empty or missing"
fi

SSH_KEY_COUNT=$(ls /etc/ssh/ssh_host_*_key 2>/dev/null | wc -l || echo 0)
if [[ ${SSH_KEY_COUNT} -ge 2 ]]; then
  check_pass "boot" "SSH host keys present" "${SSH_KEY_COUNT} key pairs"
else
  check_fail "boot" "SSH host keys missing" "Found ${SSH_KEY_COUNT}, expected ≥2"
fi

KERNEL_ERRORS=$(dmesg 2>/dev/null | grep -ciE "(panic|oops|bug:|rcu.*stall)" || echo 0)
if [[ ${KERNEL_ERRORS} -eq 0 ]]; then
  check_pass "boot" "Kernel boot clean" "No panics or oops"
else
  check_warn "boot" "Kernel issues detected" "${KERNEL_ERRORS} error(s) in dmesg"
fi

# ─── Phase 2: Service Health ───

phase_header 2 "Service Health"

check_service() {
  local svc="$1" required="${2:-true}"
  local state
  state=$(systemctl is-active "${svc}" 2>/dev/null) || state="unknown"
  if [[ "${state}" == "active" ]]; then
    check_pass "services" "${svc}" "${state}"
  elif [[ "${required}" == "true" ]]; then
    check_fail "services" "${svc}" "${state}"
  else
    check_warn "services" "${svc}" "${state} (optional)"
  fi
}

check_service "ng-gateway.service" "true"

AP_EXCLUSIVE=$(grep 'AP_EXCLUSIVE=' /etc/ng-gateway/ap-env 2>/dev/null | cut -d= -f2 | tr -d '"' || echo "unknown")
if [[ "${AP_EXCLUSIVE}" == "true" ]]; then
  check_service "ng-gateway-ap-auto.service" "false"
  check_service "ng-gateway-hostapd.service" "false"
  check_service "ng-gateway-dnsmasq.service" "false"
else
  check_service "ng-gateway-hostapd.service" "true"
  check_service "ng-gateway-dnsmasq.service" "true"
fi

HEALTH_STATUS=$(curl -fsS -o /dev/null -w "%{http_code}" http://127.0.0.1:8978/health 2>/dev/null || echo "000")
if [[ "${HEALTH_STATUS}" == "200" ]]; then
  check_pass "services" "HTTP health check" "HTTP ${HEALTH_STATUS}"
else
  check_fail "services" "HTTP health check" "HTTP ${HEALTH_STATUS} (expected 200)"
fi

# ─── Phase 3: Network ───

phase_header 3 "Network"

AP_SSID=$(grep '^ssid=' /etc/ng-gateway/hostapd.conf 2>/dev/null | cut -d= -f2 || echo "")
if [[ -n "${AP_SSID}" ]]; then
  check_pass "network" "AP SSID configured" "${AP_SSID}"
else
  check_warn "network" "AP SSID not configured" "No hostapd.conf or missing ssid="
fi

AP_IFACE=$(grep 'AP_IFACE=' /etc/ng-gateway/ap-env 2>/dev/null | cut -d= -f2 | tr -d '"' || echo "")
if [[ -n "${AP_IFACE}" ]]; then
  AP_IP=$(ip -4 addr show "${AP_IFACE}" 2>/dev/null | grep -oP 'inet \K[\d.]+' | head -1 || echo "")
  if [[ -n "${AP_IP}" ]]; then
    check_pass "network" "AP interface IP" "${AP_IFACE} = ${AP_IP}"
  else
    check_warn "network" "AP interface has no IP" "${AP_IFACE} (AP may be stopped in exclusive mode)"
  fi
fi

ETH_IFACE=$(ip -o link show 2>/dev/null | awk -F': ' '/eth[0-9]|enp/{print $2; exit}' || echo "")
if [[ -n "${ETH_IFACE}" ]]; then
  ETH_IP=$(ip -4 addr show "${ETH_IFACE}" 2>/dev/null | grep -oP 'inet \K[\d.]+' | head -1 || echo "none")
  ETH_STATE=$(cat "/sys/class/net/${ETH_IFACE}/operstate" 2>/dev/null || echo "unknown")
  if [[ "${ETH_STATE}" == "up" ]]; then
    check_pass "network" "Ethernet interface" "${ETH_IFACE} ${ETH_STATE} (IP: ${ETH_IP})"
  else
    check_warn "network" "Ethernet interface" "${ETH_IFACE} ${ETH_STATE} (cable may not be connected)"
  fi
fi

if command -v nslookup >/dev/null 2>&1; then
  if nslookup example.com >/dev/null 2>&1; then
    check_pass "network" "DNS resolution" "example.com resolved"
  else
    check_warn "network" "DNS resolution failed" "May not have internet access (normal for factory)"
  fi
fi

# ─── Phase 4: Data Integrity ───

phase_header 4 "Data Integrity"

for dir in /var/lib/ng-gateway /var/lib/ng-gateway/data /var/lib/ng-gateway/drivers \
           /var/lib/ng-gateway/plugins /etc/ng-gateway /opt/ng-gateway/bin; do
  if [[ -d "${dir}" ]]; then
    check_pass "data" "Directory exists" "${dir}"
  else
    check_fail "data" "Directory missing" "${dir}"
  fi
done

if [[ -x /opt/ng-gateway/bin/ng-gateway-bin ]]; then
  check_pass "data" "Gateway binary" "executable"
else
  check_fail "data" "Gateway binary" "missing or not executable"
fi

if [[ -f /etc/ng-gateway/gateway.toml ]]; then
  check_pass "data" "Gateway config" "/etc/ng-gateway/gateway.toml"
else
  check_fail "data" "Gateway config missing"
fi

EMMC_ERRORS=$(dmesg 2>/dev/null | grep -ciE "(mmcblk.*error|mmcblk.*fail|I/O error)" || echo 0)
if [[ ${EMMC_ERRORS} -eq 0 ]]; then
  check_pass "data" "eMMC health" "No I/O errors in dmesg"
else
  check_fail "data" "eMMC health" "${EMMC_ERRORS} I/O error(s) in dmesg"
fi

ROOT_USAGE=$(df -h / 2>/dev/null | awk 'NR==2{print $5}' || echo "unknown")
check_pass "data" "Disk usage" "${ROOT_USAGE}"

# ─── Phase 5: Uniqueness ───

phase_header 5 "Uniqueness"

if [[ -n "${GOLDEN_MACHINE_ID}" ]]; then
  CURRENT_MID=$(cat /etc/machine-id 2>/dev/null || echo "")
  if [[ "${CURRENT_MID}" != "${GOLDEN_MACHINE_ID}" ]] && [[ -n "${CURRENT_MID}" ]]; then
    check_pass "uniqueness" "machine-id differs from golden" "${CURRENT_MID}"
  elif [[ "${CURRENT_MID}" == "${GOLDEN_MACHINE_ID}" ]]; then
    check_fail "uniqueness" "machine-id same as golden sample" "Identity collision!"
  fi
else
  check_warn "uniqueness" "No golden machine-id provided" "Use --golden-machine-id to verify"
fi

# ─── Summary ───

if [[ ${JSON_MODE} -eq 0 ]]; then
  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "QA Verification Summary"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo -e "  ${GREEN}PASS${NC}: ${TOTAL_PASS}"
  echo -e "  ${RED}FAIL${NC}: ${TOTAL_FAIL}"
  echo -e "  ${YELLOW}WARN${NC}: ${TOTAL_WARN}"
  echo ""
  if [[ ${TOTAL_FAIL} -eq 0 ]]; then
    echo -e "  ${GREEN}Overall: PASS — device is ready for shipment${NC}"
  else
    echo -e "  ${RED}Overall: FAIL — ${TOTAL_FAIL} check(s) failed, investigate before shipping${NC}"
  fi
  echo ""
else
  echo "{"
  echo "  \"summary\": {"
  echo "    \"pass\": ${TOTAL_PASS},"
  echo "    \"fail\": ${TOTAL_FAIL},"
  echo "    \"warn\": ${TOTAL_WARN},"
  echo "    \"overall\": \"$([ ${TOTAL_FAIL} -eq 0 ] && echo PASS || echo FAIL)\","
  echo "    \"timestamp\": \"$(date -Iseconds)\","
  echo "    \"machine_id\": \"$(cat /etc/machine-id 2>/dev/null || echo '')\","
  echo "    \"hostname\": \"$(hostname 2>/dev/null || echo '')\""
  echo "  },"
  echo "  \"checks\": ["
  for i in "${!RESULTS[@]}"; do
    if [[ $i -lt $((${#RESULTS[@]} - 1)) ]]; then
      echo "    ${RESULTS[$i]},"
    else
      echo "    ${RESULTS[$i]}"
    fi
  done
  echo "  ]"
  echo "}"
fi

[[ ${TOTAL_FAIL} -eq 0 ]]
