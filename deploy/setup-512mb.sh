#!/usr/bin/env bash
# Non-destructive 512MB / 10GB SSD host helper for Vultr Debian 13
# Explicitly preserves all existing Xray (443), WireGuard, sysctl, and Demo (8080) settings!
set -euo pipefail

echo "[1/3] Checking Swap airbag (non-destructive)..."
SWAP_TOTAL="$(awk '/SwapTotal/ {print $2}' /proc/meminfo)"
DISK_AVAIL_KB="$(df -k / | awk 'NR==2 {print $4}')"
if [[ "${SWAP_TOTAL}" -eq 0 && "${DISK_AVAIL_KB}" -gt 3145728 && ! -f /swapfile-keepdays ]]; then
  fallocate -l 1G /swapfile-keepdays || dd if=/dev/zero of=/swapfile-keepdays bs=1M count=1024 status=none
  chmod 600 /swapfile-keepdays
  mkswap /swapfile-keepdays
  swapon /swapfile-keepdays
  grep -q '/swapfile-keepdays' /etc/fstab || echo '/swapfile-keepdays none swap sw 0 0' >> /etc/fstab
  echo "  -> Created 1GB /swapfile-keepdays"
else
  echo "  -> Existing Swap (${SWAP_TOTAL} KB) or low disk (${DISK_AVAIL_KB} KB), skipping."
fi

echo "[2/3] Capping journald log size to 80MB (protects 10GB SSD)..."
mkdir -p /etc/systemd/journald.conf.d
cat <<EOF >/etc/systemd/journald.conf.d/keepdays-cap.conf
[Journal]
SystemMaxUse=80M
RuntimeMaxUse=32M
EOF
systemctl restart systemd-journald

echo "[3/3] Creating persistent /var/lib/keepdays directories..."
id -u keepdays >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin keepdays
mkdir -p /var/lib/keepdays/db /var/lib/keepdays/uploads /etc/keepdays /opt/keepdays/dist
chown -R keepdays:keepdays /var/lib/keepdays

echo "Done! Xray (443), WireGuard, and Demo (8080) remain completely untouched."
