# syntax=docker/dockerfile:1.7
# =============================================================================
# KeepDays Cross-Platform Debian 13 (Trixie) Production Artifact Builder
#
# Usage from macOS (Apple Silicon -> Vultr x86_64 or arm64):
#   docker buildx build --platform linux/amd64 \
#     -f deploy/Dockerfile.builder \
#     --output type=local,dest=./deploy/out/amd64 .
# =============================================================================

# -----------------------------------------------------------------------------
# Stage 1: Frontend Static Builder (Node 22)
# -----------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS frontend-builder
WORKDIR /build/frontend
COPY frontend/ ./

RUN if [ -f package-lock.json ]; then \
      npm ci --no-audit --no-fund && npm run build; \
    elif [ -f package.json ]; then \
      npm install --no-audit --no-fund && npm run build; \
    else \
      mkdir -p dist && \
      printf '<!doctype html><html><head><meta charset="utf-8"><title>KeepDays</title></head><body><div id="root">KeepDays API Ready</div></body></html>' > dist/index.html; \
    fi

# -----------------------------------------------------------------------------
# Stage 2: Rust Release Builder on Debian GNU/Linux 13 (Trixie)
# Compiles `keepdays-server` unified with `agent-kernel`
# -----------------------------------------------------------------------------
FROM rust:1-slim-trixie AS rust-builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config build-essential cmake git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY agent-kernel ./agent-kernel
COPY backend ./backend

WORKDIR /build/backend
RUN cargo build --release --bin keepdays-server

# -----------------------------------------------------------------------------
# Stage 3: Debian 13 (Trixie) Clean Runtime Verification
# Verifies glibc symbols (`ldd`) & runs `--self-check` on target CPU architecture
# -----------------------------------------------------------------------------
FROM debian:trixie-slim AS trixie-verifier
COPY --from=rust-builder /build/agent-kernel/target/release/keepdays-server /verify/keepdays-server
RUN chmod +x /verify/keepdays-server && \
    echo "=== Verifying shared library links on Debian 13 (Trixie) ===" && \
    ldd /verify/keepdays-server && \
    echo "=== Running KeepDays binary self-check ===" && \
    /verify/keepdays-server --self-check

# -----------------------------------------------------------------------------
# Stage 4: Minimal Artifact Export (Binary + Static Frontend Assets Only)
# -----------------------------------------------------------------------------
FROM scratch AS exporter
COPY --from=trixie-verifier /verify/keepdays-server /keepdays-server
COPY --from=frontend-builder /build/frontend/dist /dist
