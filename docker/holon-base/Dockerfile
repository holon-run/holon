FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive
ARG YQ_VERSION=v4.44.3
ARG TARGETARCH

LABEL org.opencontainers.image.title="Holon Base Image"
LABEL org.opencontainers.image.description="General-purpose base image for Holon runtime fallback"
LABEL org.opencontainers.image.source="https://github.com/holon-run/holon"

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      bash \
      ca-certificates \
      curl \
      wget \
      gnupg \
      lsb-release \
      tzdata \
      locales \
      git \
      openssh-client \
      patch \
      diffutils \
      make \
      build-essential \
      python3 \
      python3-pip \
      python3-venv \
      nodejs \
      npm \
      jq \
      ripgrep \
      fd-find \
      file \
      less \
      tree \
      rsync \
      zip \
      unzip \
      xz-utils \
      gh && \
    locale-gen en_US.UTF-8 && \
    update-ca-certificates && \
    rm -rf /var/lib/apt/lists/*

RUN case "${TARGETARCH}" in \
      amd64) YQ_ARCH=amd64 ;; \
      arm64) YQ_ARCH=arm64 ;; \
      "") YQ_ARCH="$(dpkg --print-architecture)"; [ "${YQ_ARCH}" = "amd64" ] || [ "${YQ_ARCH}" = "arm64" ] ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}" && exit 1 ;; \
    esac && \
    curl -fsSL "https://github.com/mikefarah/yq/releases/download/${YQ_VERSION}/yq_linux_${YQ_ARCH}" -o /usr/local/bin/yq && \
    chmod +x /usr/local/bin/yq

ENV LANG=en_US.UTF-8
ENV LC_ALL=en_US.UTF-8
ENV TZ=UTC

WORKDIR /root
