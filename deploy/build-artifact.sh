#!/usr/bin/env bash
# =============================================================================
# KeepDays Local macOS -> Debian 13 (Trixie) Docker Buildx Packager
#
# Usage:
#   ./deploy/build-artifact.sh amd64          # Build for x86_64 Linux
#   ./deploy/build-artifact.sh arm64          # Build for aarch64 Linux
#   ./deploy/build-artifact.sh auto           # Probe remote server via `uname -m` first!
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

TARGET_ARG="${1:-amd64}"
SSH_TARGET="${DEPLOY_SSH:-}"

if [[ "${TARGET_ARG}" == "auto" ]]; then
  if [[ -z "${SSH_TARGET}" ]]; then
    echo "❌ Please set DEPLOY_SSH=root@<YOUR_SERVER_IP> when using 'auto'" >&2
    exit 1
  fi
  echo "🔍 Probing remote CPU architecture on ${SSH_TARGET} via 'uname -m'..."
  REMOTE_UNAME="$(ssh -o ConnectTimeout=8 "${SSH_TARGET}" "uname -m" | tr -d '[:space:]')"
  echo "✅ Remote server reports architecture: ${REMOTE_UNAME}"
  case "${REMOTE_UNAME}" in
    x86_64|amd64) TARGET_ARG="amd64" ;;
    aarch64|arm64) TARGET_ARG="arm64" ;;
    *)
      echo "❌ Unsupported remote CPU architecture: ${REMOTE_UNAME}" >&2
      exit 1
      ;;
  esac
fi

case "${TARGET_ARG}" in
  amd64|x86_64)
    DOCKER_PLATFORM="linux/amd64"
    EXPECTED_UNAME="x86_64"
    ARCH_TAG="amd64"
    ;;
  arm64|aarch64)
    DOCKER_PLATFORM="linux/arm64"
    EXPECTED_UNAME="aarch64"
    ARCH_TAG="arm64"
    ;;
  *)
    echo "Usage: $0 [amd64|arm64|auto]" >&2
    exit 1
    ;;
esac

STAGE_DIR="${SCRIPT_DIR}/out/${ARCH_TAG}"
BUNDLE_TAR="${SCRIPT_DIR}/out/keepdays-debian13-${ARCH_TAG}.tar.gz"

rm -rf "${STAGE_DIR}"
mkdir -p "${STAGE_DIR}"

echo "🐳 Building KeepDays Release inside Debian 13 (Trixie) container for ${DOCKER_PLATFORM}..."
echo "   (Note: macOS native binaries are NEVER used; explicit --platform=${DOCKER_PLATFORM} enforced)"

docker buildx build \
  --platform "${DOCKER_PLATFORM}" \
  --file "${SCRIPT_DIR}/Dockerfile.builder" \
  --output "type=local,dest=${STAGE_DIR}" \
  "${PROJECT_ROOT}"

chmod +x "${STAGE_DIR}/keepdays-server"
echo "${EXPECTED_UNAME}" > "${STAGE_DIR}/ARCH.txt"
cp "${SCRIPT_DIR}/keepdays.service" "${STAGE_DIR}/keepdays.service"
cp "${SCRIPT_DIR}/keepdays.env.production" "${STAGE_DIR}/keepdays.env.production"

tar -C "${STAGE_DIR}" -czf "${BUNDLE_TAR}" \
  keepdays-server \
  dist \
  ARCH.txt \
  keepdays.service \
  keepdays.env.production

SIZE_HUMAN="$(du -h "${BUNDLE_TAR}" | awk '{print $1}')"
echo ""
echo "🎉 Artifact packaged & Debian-13 verified successfully!"
echo "   📦 Bundle : ${BUNDLE_TAR} (${SIZE_HUMAN})"
echo "   🖥️  Target : ${DOCKER_PLATFORM} (uname -m == ${EXPECTED_UNAME})"
