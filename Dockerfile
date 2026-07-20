# syntax=docker/dockerfile:1.7

FROM node:24-bookworm-slim AS web-builder

WORKDIR /src/web-gui/app
COPY web-gui/app/package.json web-gui/app/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm,sharing=locked \
    npm ci
COPY web-gui/app/ ./
RUN npm run build

FROM rust:bookworm AS rust-builder

WORKDIR /src
COPY . .
COPY --from=web-builder /src/web-gui/app/dist ./web-gui/app/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git/db,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked \
    && cp target/release/holon /tmp/holon

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        git \
        gzip \
        openssh-client \
        tar \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --home-dir /var/lib/holon --uid 10001 holon \
    && mkdir -p /workspace \
    && chown holon:holon /workspace

COPY --from=rust-builder /tmp/holon /usr/local/bin/holon

ENV HOME=/var/lib/holon \
    HOLON_HOME=/var/lib/holon \
    HOLON_WORKSPACE_DIR=/workspace

WORKDIR /workspace
USER holon
EXPOSE 7878

ENTRYPOINT ["holon"]
CMD ["serve", "--listen", "0.0.0.0:7878"]
