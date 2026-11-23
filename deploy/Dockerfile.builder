# syntax=docker/dockerfile:1.7
# =============================================================================
# KeepDays Fast Native Cross-Compiler for Debian 13 (Trixie) Production Release
#
# Runs `rustc` natively on $BUILDPLATFORM (e.g. Apple Silicon arm64 at 100% M-chip speed!)
# targeting Debian 13 `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`,
# then verifies `ldd` & `--self-check` inside real target-arch `debian:trixie-slim`.
# =============================================================================

# -----------------------------------------------------------------------------
# Stage 1: Frontend Static Builder (Native $BUILDPLATFORM Node 22)
# -----------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS frontend-builder
WORKDIR /build/frontend
COPY frontend/ ./

RUN if [ -f package-lock.json ]; then \
      npm ci --no-audit --no-fund && npm run build; \
    else \
      npm install --no-audit --no-fund && npm run build; \
    fi

# -----------------------------------------------------------------------------
# Stage 2: Native $BUILDPLATFORM Rust Release Builder on Debian 13 (Trixie)
# -----------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM rust:1-slim-trixie AS rust-builder
ARG TARGETARCH
WORKDIR /build

# Force HTTPS on Debian sources + retries to avoid plain-HTTP proxy 502s
RUN sed -i 's|http://|https://|g' /etc/apt/sources.list.d/debian.sources 2>/dev/null || true && \
    apt-get -o Acquire::Retries=5 update && \
    apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
      pkg-config build-essential cmake git ca-certificates \
      gcc-x86-64-linux-gnu g++-x86-64-linux-gnu libc6-dev-amd64-cross \
      gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu

ENV CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++ \
    AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CARGO_NET_RETRY=5

COPY agent-kernel ./agent-kernel
COPY backend ./backend

WORKDIR /build/backend
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/agent-kernel/target,id=keepdays-cargo-target-${TARGETARCH} \
    set -eux; \
    if [ "${TARGETARCH}" = "arm64" ]; then \
      RUST_TARGET="aarch64-unknown-linux-gnu"; \
    else \
      RUST_TARGET="x86_64-unknown-linux-gnu"; \
    fi; \
    cargo build --release --target "${RUST_TARGET}" --bin keepdays-server && \
    cp "/build/agent-kernel/target/${RUST_TARGET}/release/keepdays-server" /build/keepdays-server-bin

# -----------------------------------------------------------------------------
# Stage 3: Target-Arch Debian 13 (Trixie) Clean Runtime Verification
# Verifies glibc symbols (`ldd`) & runs `--self-check` on target CPU architecture
# -----------------------------------------------------------------------------
FROM debian:trixie-slim AS trixie-verifier
COPY --from=rust-builder /build/keepdays-server-bin /verify/keepdays-server
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
