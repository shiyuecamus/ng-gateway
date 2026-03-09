#!/usr/bin/env bash
#
# Scan 192.168.66.* for SSH hosts accessible with orangepi/orangepi.
# Requires: sshpass (brew install sshpass on macOS)
#

set -e

NETWORK="${NETWORK:-192.168.66}"
SSH_USER="${SSH_USER:-orangepi}"
SSH_PASS="${SSH_PASS:-orangepi}"
PORT="${PORT:-22}"
TIMEOUT=1

if ! command -v sshpass &>/dev/null; then
  echo "Error: sshpass is required. Install with: brew install sshpass"
  exit 1
fi

echo "[$(date '+%H:%M:%S')] Start scanning ${NETWORK}.1 - ${NETWORK}.254"
echo "[$(date '+%H:%M:%S')] Credentials: ${SSH_USER} / ****"
echo "[$(date '+%H:%M:%S')] Timeout: ${TIMEOUT}s per host"
echo "----------------------------------------"

ok_count=0
fail_count=0

for i in $(seq 1 254); do
  ip="${NETWORK}.${i}"
  ret=0
  output=$(sshpass -p "${SSH_PASS}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=${TIMEOUT} \
    -o BatchMode=no -p "${PORT}" "${SSH_USER}@${ip}" "exit" 2>&1) || ret=$?

  if [ "$ret" -eq 0 ]; then
    echo "[$(date '+%H:%M:%S')]   OK: ${ip}"
    ((ok_count++)) || true
  else
    err=$(echo "$output" | grep -oE "Connection refused|Connection timed out|Permission denied|No route to host|Connection reset|Network is unreachable" | head -1)
    [ -z "$err" ] && err=$(echo "$output" | head -1 | cut -c1-60)
    [ -z "$err" ] && err="connection failed"
    echo "[$(date '+%H:%M:%S')] FAIL: ${ip} - ${err}"
    ((fail_count++)) || true
  fi
done

echo "----------------------------------------"
echo "[$(date '+%H:%M:%S')] Done. OK: ${ok_count}, FAIL: ${fail_count}"
