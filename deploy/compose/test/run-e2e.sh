#!/usr/bin/env bash
# ============================================================================
# E2E Test Runner for AI Vision Pipeline
#
# Prerequisites:
#   - Docker Compose services running (docker compose up -d)
#   - curl, jq installed
#
# Test matrix:
#   1. Gateway health check
#   2. AI engine status API
#   3. AI model listing API
#   4. AI pipeline listing API
#   5. AI preprocessor / postprocessor listing
#   6. Snapshot API (404 when no pipeline active)
#   7. Backpressure under load (reconnection resilience)
#
# Usage:
#   ./run-e2e.sh [--gateway-url http://localhost:5678]
# ============================================================================

set -euo pipefail

# ── Configuration ─────────────────────────────────────────────────
GATEWAY_URL="${1:-http://localhost:5678}"
API_PREFIX="${GATEWAY_URL}/api"
PASS=0
FAIL=0
TOTAL=0

# ANSI colours
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
NC='\033[0m'

# ── Helpers ───────────────────────────────────────────────────────

log_info()  { echo -e "${YELLOW}[INFO]${NC}  $*"; }
log_pass()  { echo -e "${GREEN}[PASS]${NC}  $*"; ((PASS++)); ((TOTAL++)); }
log_fail()  { echo -e "${RED}[FAIL]${NC}  $*"; ((FAIL++)); ((TOTAL++)); }

# HTTP GET → assert status code
assert_status() {
    local desc="$1"
    local url="$2"
    local expected_code="${3:-200}"
    local actual_code

    actual_code=$(curl -s -o /dev/null -w "%{http_code}" "$url" 2>/dev/null) || true
    if [ "$actual_code" = "$expected_code" ]; then
        log_pass "$desc (HTTP $actual_code)"
    else
        log_fail "$desc — expected HTTP $expected_code, got $actual_code"
    fi
}

# HTTP GET → assert JSON field value
assert_json_field() {
    local desc="$1"
    local url="$2"
    local jq_expr="$3"
    local expected="$4"
    local body actual

    body=$(curl -s "$url" 2>/dev/null) || true
    actual=$(echo "$body" | jq -r "$jq_expr" 2>/dev/null) || true

    if [ "$actual" = "$expected" ]; then
        log_pass "$desc ($jq_expr = $actual)"
    else
        log_fail "$desc — expected $jq_expr = $expected, got '$actual'"
    fi
}

# HTTP GET → assert JSON field is numeric and >= threshold
assert_json_gte() {
    local desc="$1"
    local url="$2"
    local jq_expr="$3"
    local min="$4"
    local body actual

    body=$(curl -s "$url" 2>/dev/null) || true
    actual=$(echo "$body" | jq -r "$jq_expr" 2>/dev/null) || true

    if [ -n "$actual" ] && [ "$actual" != "null" ] && [ "$(echo "$actual >= $min" | bc -l)" = "1" ]; then
        log_pass "$desc ($jq_expr = $actual >= $min)"
    else
        log_fail "$desc — expected $jq_expr >= $min, got '$actual'"
    fi
}

# ── Wait for gateway ──────────────────────────────────────────────

log_info "Waiting for gateway at $GATEWAY_URL ..."
for i in $(seq 1 30); do
    if curl -sf "${GATEWAY_URL}/health" >/dev/null 2>&1; then
        log_info "Gateway is ready (attempt $i)."
        break
    fi
    if [ "$i" -eq 30 ]; then
        log_fail "Gateway did not become ready within 30s"
        exit 1
    fi
    sleep 1
done

echo ""
log_info "═══════════════════════════════════════════════════════════"
log_info " E2E Test Suite — AI Vision Pipeline"
log_info "═══════════════════════════════════════════════════════════"
echo ""

# ── 1. Health Check ───────────────────────────────────────────────

assert_status "Gateway health endpoint" "${GATEWAY_URL}/health"

# ── 2. AI Engine Status ──────────────────────────────────────────

assert_status "AI engine status endpoint" "${API_PREFIX}/ai/engine/status"
assert_json_field "AI engine is enabled" "${API_PREFIX}/ai/engine/status" ".data.enabled" "true"
assert_json_field "Execution provider is cpu" "${API_PREFIX}/ai/engine/status" ".data.execution_provider" "cpu"
assert_json_gte "Max concurrent >= 1" "${API_PREFIX}/ai/engine/status" ".data.inference.max_concurrent" "1"
assert_json_gte "Uptime > 0" "${API_PREFIX}/ai/engine/status" ".data.uptime_secs" "0"

# ── 3. Model Listing ─────────────────────────────────────────────

assert_status "AI models endpoint" "${API_PREFIX}/ai/models"
assert_json_field "Models response code 0" "${API_PREFIX}/ai/models" ".code" "0"

# ── 4. Pipeline Listing ──────────────────────────────────────────

assert_status "AI pipelines endpoint" "${API_PREFIX}/ai/pipelines"
assert_json_field "Pipelines response code 0" "${API_PREFIX}/ai/pipelines" ".code" "0"

# ── 5. Processor Listings ─────────────────────────────────────────

assert_status "AI preprocessors endpoint" "${API_PREFIX}/ai/processors/pre"
assert_json_gte "At least 3 preprocessors" "${API_PREFIX}/ai/processors/pre" ".data | length" "3"

assert_status "AI postprocessors endpoint" "${API_PREFIX}/ai/processors/post"
assert_json_gte "At least 4 postprocessors" "${API_PREFIX}/ai/processors/post" ".data | length" "4"

# ── 6. Snapshot (no active pipeline → expect error) ──────────────

assert_status "Snapshot without pipeline → 404" "${API_PREFIX}/ai/channels/99999/snapshot" "404"

# ── 7. RTSP Test Stream Availability ─────────────────────────────

log_info "Checking RTSP test stream availability..."
if command -v ffprobe &>/dev/null; then
    if timeout 10 ffprobe -v quiet -i "rtsp://localhost:8554/test-cam" -rtsp_transport tcp 2>/dev/null; then
        log_pass "RTSP test stream is live at rtsp://localhost:8554/test-cam"
    else
        log_fail "RTSP test stream not reachable (ffprobe failed)"
    fi
else
    log_info "ffprobe not found, skipping RTSP stream check"
fi

# ── 8. Driver Reconnection Resilience ────────────────────────────
# This test verifies that killing the RTSP server and restarting it
# doesn't crash the gateway. The gateway should detect disconnection
# and attempt reconnection via its supervision loop.

log_info "Testing driver reconnection resilience..."
log_info "  Stopping RTSP server..."
docker stop ng-test-rtsp >/dev/null 2>&1 || true
sleep 3

# Gateway should still respond to API calls.
assert_status "Gateway API responds after RTSP disconnect" "${API_PREFIX}/ai/engine/status"

log_info "  Restarting RTSP server..."
docker start ng-test-rtsp >/dev/null 2>&1 || true
sleep 5

assert_status "Gateway API responds after RTSP reconnect" "${API_PREFIX}/ai/engine/status"

# ── Summary ───────────────────────────────────────────────────────

echo ""
log_info "═══════════════════════════════════════════════════════════"
if [ "$FAIL" -eq 0 ]; then
    echo -e "${GREEN}  ALL $TOTAL TESTS PASSED${NC}"
else
    echo -e "${RED}  $FAIL / $TOTAL TESTS FAILED${NC}"
fi
log_info "═══════════════════════════════════════════════════════════"
echo ""

exit "$FAIL"
