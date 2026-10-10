# syntax=docker/dockerfile:1

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

# ---- runtime: slim Debian (labs need bash, curl, openssl, sqlite3; see docs/decisions.md) ----
FROM ${RUNTIME_IMAGE} AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates curl openssl sqlite3 procps \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash pet \
    && useradd --create-home --shell /bin/bash learner \
    && mkdir -p /var/lib/bramble /home/learner/lab \
    && chown pet:pet /var/lib/bramble \
    && chown learner:learner /home/learner/lab
COPY --from=builder /app/target/release/bramble /usr/local/bin/bramble
COPY content /usr/share/bramble/content
ENV BRAMBLE_DATA_DIR=/var/lib/bramble \
    BRAMBLE_CONTENT_DIR=/usr/share/bramble/content
USER pet
WORKDIR /var/lib/bramble
ENTRYPOINT ["bramble"]
CMD ["whoami"]
