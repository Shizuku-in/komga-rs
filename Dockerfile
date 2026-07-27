# syntax=docker/dockerfile:1.7

ARG NODE_IMAGE=node:24-bookworm-slim
ARG UBUNTU_IMAGE=ubuntu:24.04

FROM --platform=$BUILDPLATFORM ${NODE_IMAGE} AS webui

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build/webui
COPY webui/package.json webui/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci
COPY webui/ ./
RUN npm run build

FROM ${UBUNTU_IMAGE} AS builder

ARG RUST_VERSION=1.96.1
ARG TARGETARCH

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        libheif-dev \
        pkg-config \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/* \
    && curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
        https://sh.rustup.rs --output /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain "${RUST_VERSION}" \
    && rm /tmp/rustup-init.sh \
    && rustc --version

WORKDIR /build
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates/ ./crates/
COPY --from=webui /build/webui/dist ./webui/dist/

ENV KOMGA_WEBUI_DIST_DIR=webui/dist

RUN --mount=type=cache,id=komga-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=komga-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=komga-cargo-target-${TARGETARCH},target=/build/target \
    cargo build --locked --release -p komga-server --features heif-native \
    && install -Dm755 target/release/komga-server /out/komga-server

FROM ${UBUNTU_IMAGE} AS runtime

ARG KOMGA_UID=1000
ARG KOMGA_GID=1000

# Ubuntu 24.04 may already reserve UID/GID 1000, so reuse those identities
# instead of creating duplicate numeric IDs.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates \
        curl \
        libheif1 \
        libheif-plugin-dav1d \
        libheif-plugin-libde265 \
    && rm -rf /var/lib/apt/lists/* \
    && current_group="$(getent group "${KOMGA_GID}" | cut -d: -f1)" \
    && if [ -z "${current_group}" ]; then \
        groupadd --gid "${KOMGA_GID}" komga; \
    elif [ "${current_group}" != komga ]; then \
        groupmod --new-name komga "${current_group}"; \
    fi \
    && current_user="$(getent passwd "${KOMGA_UID}" | cut -d: -f1)" \
    && if [ -z "${current_user}" ]; then \
        useradd --uid "${KOMGA_UID}" --gid "${KOMGA_GID}" \
            --home-dir /config --shell /usr/sbin/nologin komga; \
    elif [ "${current_user}" != komga ]; then \
        usermod --login komga --gid "${KOMGA_GID}" \
            --home /config --shell /usr/sbin/nologin "${current_user}"; \
    else \
        usermod --gid "${KOMGA_GID}" --home /config \
            --shell /usr/sbin/nologin komga; \
    fi \
    && install -d -o "${KOMGA_UID}" -g "${KOMGA_GID}" /config

COPY --from=builder /out/komga-server /usr/local/bin/komga-server
COPY LICENSE /usr/share/licenses/komga-rs/
COPY crates/komga-media/src/kepub.rs /usr/share/licenses/komga-rs/kepubify-attribution.rs
COPY crates/komga-server/assets/licenses/ /usr/share/licenses/komga-rs/assets/
COPY webui/src/styles/readium/LICENSE /usr/share/licenses/komga-rs/Readium-CSS-LICENSE

ENV HOME=/config \
    KOMGA_CONFIG_DIR=/config \
    KOMGA_HOST=0.0.0.0 \
    KOMGA_PORT=25600

LABEL org.opencontainers.image.title="komga-rs" \
      org.opencontainers.image.description="Rust port of the Komga media server" \
      org.opencontainers.image.licenses="MIT"

VOLUME ["/config"]
EXPOSE 25600
STOPSIGNAL SIGTERM

HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=5 \
    CMD curl --fail --silent --show-error \
        "http://127.0.0.1:${KOMGA_PORT:-25600}${KOMGA_CONTEXT_PATH:-}/actuator/health" \
        >/dev/null || exit 1

USER komga
ENTRYPOINT ["/usr/local/bin/komga-server"]
