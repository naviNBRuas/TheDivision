# Shared image for divisi's opt-in Docker execution backend (see
# `divisi agent docker enable`, `divisi-runtime::docker`). One image with
# every supported agent CLI installed, rather than an image per agent —
# simpler to build/maintain, works for any agent out of the box.
#
# Build (from the repo root): docker build -t divisi-agents -f docker/Dockerfile .
#
# Stage 1 builds `divisi` just to run its own
# `divisi internal print-bootstrap-script`, which prints a shell script
# derived from `divisi_core::registry::builtin_registry()`'s real,
# verified `bootstrap_install` commands — the same ones `divisi setup`
# runs on a host install. That keeps this image in sync with the registry
# automatically instead of a separately maintained, driftable install
# list.
#
# Known limitation, not verified end-to-end in this pass: several
# bootstrap_install commands assume an interactive, real-user environment
# (installing into `$HOME/.local/bin` and expecting shell rc files to add
# it to PATH) — under `docker build`, that's root's `/root`, and rc files
# are never sourced by `docker exec`. The PATH entries below cover the
# install locations each installer's docs/verified `bootstrap_install`
# comment (see registry.rs) describe, but not every one of the 10 agents
# was confirmed to actually land a working binary this way — some may
# need per-agent fixups (extra apt packages, an explicit install
# directory) discovered only by building and testing this image for real.
# Each install is wrapped so one agent failing doesn't break the build.

FROM rust:1-slim-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin divisi

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        curl ca-certificates git build-essential \
        python3 python3-pip python3-venv pipx \
        nodejs npm \
    && rm -rf /var/lib/apt/lists/*
RUN pipx ensurepath && pipx install uv 2>/dev/null || pip3 install --break-system-packages uv

ENV PATH="/root/.local/bin:/root/.local/share/claude/bin:/root/.npm-global/bin:${PATH}"

COPY --from=builder /build/target/release/divisi /usr/local/bin/divisi

RUN divisi internal print-bootstrap-script > /tmp/install-agents.sh \
    && sh /tmp/install-agents.sh \
    && rm /tmp/install-agents.sh

WORKDIR /root
CMD ["sleep", "infinity"]
