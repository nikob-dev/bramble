ARG RUST_IMAGE=rust:1.99.0-slim-bookworm
ARG RUNTIME_IMAGE=debian:12.15-slim

# ---- chef: toolchain + cargo-chef for dependency-layer caching ----
FROM ${RUST_IMAGE} AS chef
RUN cargo install cargo-chef --version 0.1.78 --locked
WORKDIR /app

# ---- dev: toolchain used by `docker compose run --rm dev cargo test` ----
FROM chef AS dev
RUN rustup component add rustfmt clippy

# ---- planner: compute the dependency recipe ----
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---- builder: cook deps (cached), then build the binary ----
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin bramble

# ---- web: compile the Leptos app to WebAssembly with trunk ----
FROM chef AS web
ARG TARGETARCH
ARG TRUNK_VERSION=0.21.14
# Pinned so a tampered release fails the build (from the release's .sha256 files).
ARG TRUNK_SHA256_AMD64=f2b4680cd239693a646a2795e4633c625328d7b2a044fbe749fa3a2fe9e7036b
ARG TRUNK_SHA256_ARM64=b1d8e60e454f7fc182d9a4d95d1506ffbae947d8ba90f8f6f02da93b60f980f9
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add wasm32-unknown-unknown \
    && case "$TARGETARCH" in \
         amd64) arch=x86_64; sum="$TRUNK_SHA256_AMD64" ;; \
         arm64) arch=aarch64; sum="$TRUNK_SHA256_ARM64" ;; \
         *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
       esac \
    && curl -fsSL -o /tmp/trunk.tgz \
         "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-${arch}-unknown-linux-gnu.tar.gz" \
    && echo "${sum}  /tmp/trunk.tgz" | sha256sum -c - \
    && tar -xzf /tmp/trunk.tgz -C /usr/local/bin \
    && rm /tmp/trunk.tgz \
    && trunk --version
COPY . .
RUN cd crates/bramble-web && trunk build --release

# ---- runtime: slim Debian (labs need bash, curl, openssl, sqlite3, less; see docs/decisions.md) ----
FROM ${RUNTIME_IMAGE} AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates curl less openssl sqlite3 procps \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash pet \
    && useradd --create-home --shell /bin/bash learner \
    && mkdir -p /var/lib/bramble /home/learner/lab \
    && chown pet:pet /var/lib/bramble \
    && chown learner:learner /home/learner/lab
COPY --from=builder /app/target/release/bramble /usr/local/bin/bramble
COPY content /usr/share/bramble/content
COPY --from=web /app/crates/bramble-web/dist /usr/share/bramble/web
ENV BRAMBLE_DATA_DIR=/var/lib/bramble \
    BRAMBLE_CONTENT_DIR=/usr/share/bramble/content \
    BRAMBLE_WEB_DIR=/usr/share/bramble/web
USER pet
WORKDIR /var/lib/bramble
ENTRYPOINT ["bramble"]
CMD ["whoami"]
