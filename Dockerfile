ARG RUST_VERSION=1.95.0

FROM rust:${RUST_VERSION}-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY crates ./crates

RUN cargo build --release --locked --bin eva

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system eva \
    && useradd --system --gid eva --home-dir /home/eva --create-home eva \
    && mkdir -p /workspace \
    && chown -R eva:eva /home/eva /workspace

ARG EVA_VERSION=unknown
ARG VCS_REF=unknown
ARG BUILD_DATE=unknown

LABEL org.opencontainers.image.title="Eva-CLI" \
      org.opencontainers.image.description="Eva-CLI command-line runtime" \
      org.opencontainers.image.source="https://github.com/Yetmos/Eva-CLI" \
      org.opencontainers.image.version="${EVA_VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.licenses="UNLICENSED"

COPY --from=builder /app/target/release/eva /usr/local/bin/eva

USER eva
WORKDIR /workspace
ENTRYPOINT ["eva"]
CMD ["--help"]
