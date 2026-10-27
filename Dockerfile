# syntax=docker/dockerfile:1
#
# Rivu 生产镜像：一个 rivu 二进制，加上 tracker（/js/*）与看板（其余页面）的构建产物。
#
#   docker build -t rivu --build-arg RIVU_VERSION=0.1.0 --build-arg RIVU_GIT_COMMIT=$(git rev-parse --short HEAD) .
#
# 运行方式见 docs/install.md 与 deploy/compose.yml。容器默认以非 root 用户运行 `rivu all`，
# 公共端口 8080（采集、API、看板），内部端口 9090（/healthz /readyz /metrics，只给编排与监控用，不要对外开放）。

ARG RIVU_VERSION=0.0.0-dev
ARG RIVU_GIT_COMMIT=unknown
ARG RIVU_SV=1

# ---------------------------------------------------------------- 前端：tracker 与看板
FROM node:24-bookworm-slim AS web
ARG RIVU_VERSION
ARG RIVU_SV
ENV PNPM_HOME=/pnpm PATH=/pnpm:$PATH
RUN corepack enable && corepack prepare pnpm@12.5.1 --activate
WORKDIR /src

COPY apps/tracker/package.json apps/tracker/pnpm-lock.yaml apps/tracker/pnpm-workspace.yaml apps/tracker/
RUN --mount=type=cache,target=/pnpm/store cd apps/tracker && pnpm install --frozen-lockfile
COPY apps/tracker apps/tracker
RUN cd apps/tracker && RIVU_SV=${RIVU_SV} pnpm build

COPY apps/dashboard/package.json apps/dashboard/pnpm-lock.yaml apps/dashboard/
RUN --mount=type=cache,target=/pnpm/store cd apps/dashboard && pnpm install --frozen-lockfile
COPY apps/dashboard apps/dashboard
RUN cd apps/dashboard && RIVU_VERSION=${RIVU_VERSION} pnpm build

# ---------------------------------------------------------------- rivu 二进制
FROM rust:1-bookworm AS rust
ARG RIVU_GIT_COMMIT
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    RIVU_GIT_COMMIT=${RIVU_GIT_COMMIT} cargo build --release --locked -p rivu \
    && cp target/release/rivu /usr/local/bin/rivu

# ---------------------------------------------------------------- 运行镜像
FROM debian:bookworm-slim
ARG RIVU_VERSION
LABEL org.opencontainers.image.title="Rivu" \
      org.opencontainers.image.description="Open-source, self-hosted web analytics" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${RIVU_VERSION}"

# ca-certificates：下载 GeoIP 库与数据中心网段（HTTPS）；tzdata 不需要，时区规则编译在二进制里
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /var/lib/rivu --shell /usr/sbin/nologin rivu \
    && mkdir -p /var/lib/rivu /opt/rivu \
    && chown rivu:rivu /var/lib/rivu

COPY --from=rust /usr/local/bin/rivu /usr/local/bin/rivu
COPY --from=web /src/apps/tracker/dist /opt/rivu/tracker
COPY --from=web /src/apps/dashboard/build /opt/rivu/dashboard

ENV RIVU_TRACKER_DIR=/opt/rivu/tracker \
    RIVU_DASHBOARD_DIR=/opt/rivu/dashboard \
    RIVU_DATA_DIR=/var/lib/rivu \
    RIVU_LISTEN_ADDR=0.0.0.0:8080 \
    RIVU_INTERNAL_ADDR=0.0.0.0:9090 \
    RIVU_LOG_FORMAT=json

USER rivu
VOLUME ["/var/lib/rivu"]
EXPOSE 8080 9090
HEALTHCHECK --interval=10s --timeout=6s --start-period=60s --retries=3 CMD ["rivu", "healthcheck"]
ENTRYPOINT ["rivu"]
CMD ["all"]
