#!/usr/bin/env bash
# =============================================================================
# KeepDays Safe One-Command SSH Deployer for Debian 13
#
# Safety Guarantees:
# 1. Probes remote `uname -m` first (`x86_64` -> amd64, `aarch64` -> arm64)
# 2. Auto-triggers local Docker Debian-13 build if matching bundle doesn't exist
# 3. Protects existing reverse-proxy / tunnel services on standard ports
# 4. Verifies target port (default 3090) has zero conflict via `ss -tulnp`
# 5. Stores SQLite DB & data strictly in persistent `/var/lib/keepdays`
# 6. Runs `--self-check` on server before atomic swap & health-check verification
#
# Usage:
#   DEPLOY_SSH=root@<YOUR_SERVER_IP> ./deploy/deploy-ssh.sh
#   DEPLOY_SSH=root@<YOUR_SERVER_IP> REBUILD=1 ./deploy/deploy-ssh.sh
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

SSH_TARGET="${DEPLOY_SSH:?Please set DEPLOY_SSH=root@<YOUR_SERVER_IP>}"
DEPLOY_PORT="${DEPLOY_PORT:-3090}"
REBUILD="${REBUILD:-0}"
OPEN_UFW="${OPEN_UFW:-0}"

# Guardrail #1: Never allow colliding with standard system/proxy ports
if [[ "${DEPLOY_PORT}" == "443" || "${DEPLOY_PORT}" == "8443" || "${DEPLOY_PORT}" == "2053" || "${DEPLOY_PORT}" == "49222" || "${DEPLOY_PORT}" == "8080" || "${DEPLOY_PORT}" == "18080" || "${DEPLOY_PORT}" == "53" || "${DEPLOY_PORT}" == "22" ]]; then
  echo "❌ Refusing to use reserved port ${DEPLOY_PORT}!" >&2
  exit 1
fi

echo "🔍 [Step 1/5] Inspecting target server (${SSH_TARGET})..."
REMOTE_INFO="$(ssh -o ConnectTimeout=10 "${SSH_TARGET}" '
  set -e
  ARCH=$(uname -m)
  OS_PRETTY=$(grep PRETTY_NAME /etc/os-release | cut -d= -f2 | tr -d "\"")
  MEM_MB=$(awk "/MemTotal/ {printf \"%d\", \$2/1024}" /proc/meminfo)
  DISK_FREE=$(df -h / | awk "NR==2 {print \$4}")
  echo "${ARCH}|${OS_PRETTY}|${MEM_MB}MB|${DISK_FREE}"
')"

IFS='|' read -r REMOTE_ARCH REMOTE_OS REMOTE_MEM REMOTE_DISK <<< "${REMOTE_INFO}"
echo "   • OS           : ${REMOTE_OS}"
echo "   • Architecture : ${REMOTE_ARCH}"
echo "   • Memory       : ${REMOTE_MEM} | Root Disk Free: ${REMOTE_DISK}"

case "${REMOTE_ARCH}" in
  x86_64)  ARCH_TAG="amd64" ;;
  aarch64) ARCH_TAG="arm64" ;;
  *)
    echo "❌ Unsupported server CPU architecture: ${REMOTE_ARCH}" >&2
    exit 1
    ;;
esac

BUNDLE_TAR="${SCRIPT_DIR}/out/keepdays-debian13-${ARCH_TAG}.tar.gz"

# Step 2: Build matching Linux Debian-13 bundle locally via Docker if needed
if [[ "${REBUILD}" == "1" || ! -f "${BUNDLE_TAR}" ]]; then
  echo "🐳 [Step 2/5] Building matching ${ARCH_TAG} Debian-13 bundle locally on Mac..."
  "${SCRIPT_DIR}/build-artifact.sh" "${ARCH_TAG}"
else
  echo "✅ [Step 2/5] Found existing verified bundle: ${BUNDLE_TAR} (set REBUILD=1 to force rebuild)"
fi

# Step 3: Pre-flight port & coexistence check on remote server (Xray / WireGuard / 8080)
echo "🛡️  [Step 3/5] Running port & service coexistence check (port ${DEPLOY_PORT})..."
ssh "${SSH_TARGET}" "DEPLOY_PORT=${DEPLOY_PORT} bash -s" <<'EOF'
set -euo pipefail

# Verify port is either free OR already owned by keepdays-server
PORT_OWNER="$(ss -tulnp | awk -v p=":${DEPLOY_PORT} " '$5 ~ p || $5 ~ ":"ENVIRON["DEPLOY_PORT"]"$" {print $0}' || true)"
if [[ -n "${PORT_OWNER}" ]] && ! echo "${PORT_OWNER}" | grep -q "keepdays-server"; then
  echo "❌ Port ${DEPLOY_PORT} is already occupied by another process:" >&2
  echo "   ${PORT_OWNER}" >&2
  echo "   Choose another port via DEPLOY_PORT=<port> ./deploy/deploy-ssh.sh" >&2
  exit 1
fi
echo "   • Port ${DEPLOY_PORT} check passed (443 Xray, 8080 Demo, WireGuard untouched)."
EOF

# Step 4: Upload bundle to /tmp and perform atomic installation
echo "🚀 [Step 4/5] Uploading release artifact to ${SSH_TARGET}..."
scp "${BUNDLE_TAR}" "${SSH_TARGET}:/tmp/keepdays-release.tar.gz"

echo "⚙️  [Step 5/5] Installing atomically with persistent /var/lib/keepdays storage..."
ssh "${SSH_TARGET}" "DEPLOY_PORT=${DEPLOY_PORT} OPEN_UFW=${OPEN_UFW} bash -s" <<'EOF'
set -euo pipefail

STAGE="/tmp/keepdays-stage-$$"
mkdir -p "${STAGE}"
tar -xzf /tmp/keepdays-release.tar.gz -C "${STAGE}"
rm -f /tmp/keepdays-release.tar.gz

# Verify architecture stamp inside tarball matches live server `uname -m`
EXPECTED_ARCH="$(cat "${STAGE}/ARCH.txt" | tr -d '[:space:]')"
ACTUAL_ARCH="$(uname -m)"
if [[ "${EXPECTED_ARCH}" != "${ACTUAL_ARCH}" ]]; then
  rm -rf "${STAGE}"
  echo "❌ Architecture mismatch! Bundle is for ${EXPECTED_ARCH}, server is ${ACTUAL_ARCH}" >&2
  exit 1
fi

# Create dedicated unprivileged system user if absent
if ! id -u keepdays >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin keepdays
fi

# Create persistent directories (NEVER wiped across deployments!)
mkdir -p /var/lib/keepdays/db /var/lib/keepdays/uploads /var/lib/keepdays/agent_fs /etc/keepdays /opt/keepdays
chown -R keepdays:keepdays /var/lib/keepdays

# Provision 512MB Swap safety airbag ONLY if server currently has 0 swap (touches zero WireGuard/Xray sysctls!)
SWAP_TOTAL="$(awk '/SwapTotal/ {print $2}' /proc/meminfo)"
DISK_AVAIL_KB="$(df -k / | awk 'NR==2 {print $4}')"
if [[ "${SWAP_TOTAL}" -eq 0 && "${DISK_AVAIL_KB}" -gt 3145728 && ! -f /swapfile-keepdays ]]; then
  echo "   • Provisioning 1GB Swap airbag (/swapfile-keepdays) for 512MB RAM stability..."
  fallocate -l 1G /swapfile-keepdays
  chmod 600 /swapfile-keepdays
  mkswap /swapfile-keepdays >/dev/null
  swapon /swapfile-keepdays
  grep -q '/swapfile-keepdays' /etc/fstab || echo '/swapfile-keepdays none swap sw 0 0' >> /etc/fstab
fi

# Initialize /etc/keepdays/keepdays.env ONLY on first install (never overwrite live R2 keys!)
if [[ ! -f /etc/keepdays/keepdays.env ]]; then
  echo "   • Generating initial /etc/keepdays/keepdays.env with random 64-hex secrets..."
  RAND_JWT="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
  RAND_S3="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
  sed -e "s/^PORT=.*/PORT=${DEPLOY_PORT}/" \
      -e "s|^JWT_SECRET=.*|JWT_SECRET=${RAND_JWT}|" \
      -e "s|^S3_SECRET_KEY=.*|S3_SECRET_KEY=${RAND_S3}|" \
      "${STAGE}/keepdays.env.production" > /etc/keepdays/keepdays.env
  chown root:keepdays /etc/keepdays/keepdays.env
  chmod 640 /etc/keepdays/keepdays.env
else
  sed -i -e "s/^PORT=.*/PORT=${DEPLOY_PORT}/" /etc/keepdays/keepdays.env
  grep -q '^AGENT_FS_DIR=' /etc/keepdays/keepdays.env || echo 'AGENT_FS_DIR=/var/lib/keepdays/agent_fs' >> /etc/keepdays/keepdays.env
fi

# Smoke-test new binary directly on Debian 13 host BEFORE touching live service!
chmod +x "${STAGE}/keepdays-server"
"${STAGE}/keepdays-server" --self-check

# Atomic swap of binary and frontend static bundle
mv "${STAGE}/keepdays-server" /opt/keepdays/keepdays-server.next
mv -f /opt/keepdays/keepdays-server.next /opt/keepdays/keepdays-server

rm -rf /opt/keepdays/dist.next
mv "${STAGE}/dist" /opt/keepdays/dist.next
rm -rf /opt/keepdays/dist.prev
[[ -d /opt/keepdays/dist ]] && mv /opt/keepdays/dist /opt/keepdays/dist.prev || true
mv /opt/keepdays/dist.next /opt/keepdays/dist
rm -rf /opt/keepdays/dist.prev

chown -R root:root /opt/keepdays
cp "${STAGE}/keepdays.service" /etc/systemd/system/keepdays.service
rm -rf "${STAGE}"

systemctl daemon-reload
systemctl enable keepdays.service >/dev/null
systemctl restart keepdays.service

if [[ "${OPEN_UFW}" == "1" ]] && command -v ufw >/dev/null 2>&1; then
  ufw allow "${DEPLOY_PORT}/tcp" comment 'KeepDays web' >/dev/null || true
  echo "   • UFW opened ${DEPLOY_PORT}/tcp for public web access."
fi

# Poll live HTTP health endpoint up to 15s (handles cold 1-vCPU SQLite WAL boot cleanly)
ACTIVE_PORT="$(grep -E '^PORT=' /etc/keepdays/keepdays.env | cut -d= -f2 | tr -d '[:space:]')"
ACTIVE_PORT="${ACTIVE_PORT:-3090}"
echo "   • Health check response on 127.0.0.1:${ACTIVE_PORT}:"
HEALTH_OK=0
for _ in $(seq 1 15); do
  if curl -fsS "http://127.0.0.1:${ACTIVE_PORT}/api/v1/system/health"; then
    HEALTH_OK=1
    break
  fi
  sleep 1
done
echo ""
if [[ "${HEALTH_OK}" != "1" ]]; then
  echo "❌ KeepDays service did not pass health check within 15s!" >&2
  journalctl -u keepdays -n 30 --no-pager >&2 || true
  exit 1
fi
EOF

echo ""
echo "✅ KeepDays deployed cleanly to ${SSH_TARGET}!"
echo "   • Persistent DB : /var/lib/keepdays/db/keepdays.db"
echo "   • Config file   : /etc/keepdays/keepdays.env (update S3/R2 keys here)"
echo "   • Service logs  : ssh ${SSH_TARGET} 'journalctl -u keepdays -f'"
