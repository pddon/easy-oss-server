# syntax=docker/dockerfile:1
# 构建上下文必须是项目根目录（dist 的上一级）：
#   docker build -f dist/Dockerfile -t oss-server:1.0.0 .

# ---------- 构建阶段 ----------
FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev perl make

WORKDIR /app

# 先复制清单，缓存依赖层。
COPY Cargo.toml Cargo.lock ./

# 用空壳源码预编译依赖，随后再复制真实源码，避免每次改代码都重编依赖。
RUN mkdir -p src && \
    printf '' > src/lib.rs && \
    printf 'fn main() {}' > src/main.rs && \
    cargo build --release || true

COPY migrations ./migrations
COPY src ./src

# COPY 会保留源码文件原始的修改时间，可能比上一步的预编译产物还旧，
# cargo 会据此误判“无需重新编译”，最终镜像里留下的是空壳 main。
# 触碰一次源码，确保真实代码一定被编进产物。
RUN find src -type f -exec touch {} +

# CARGO_FEATURES=mysql 可启用 MySQL 后端；默认仅 redb（无额外 C 依赖）。
ARG CARGO_FEATURES=""
RUN if [ -n "$CARGO_FEATURES" ]; then \
        cargo build --release --features "$CARGO_FEATURES"; \
    else \
        cargo build --release; \
    fi

# ---------- 运行阶段 ----------
FROM alpine:3.19

LABEL org.opencontainers.image.title="OSS Server" \
      org.opencontainers.image.description="Lightweight OSS/S3-compatible object storage server built with Rust + Axum + redb" \
      org.opencontainers.image.url="https://github.com/pddon/easy-oss-server" \
      org.opencontainers.image.source="https://github.com/pddon/easy-oss-server" \
      org.opencontainers.image.documentation="https://github.com/pddon/easy-oss-server#readme" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0" \
      org.opencontainers.image.vendor="OSS Server Team"

RUN apk add --no-cache ca-certificates tzdata su-exec && \
    rm -rf /var/cache/apk/*

WORKDIR /app

COPY --from=builder /app/target/release/oss-server /app/oss-server
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

# 数据目录：由 compose 挂载宿主机目录，容器内固定为 /data。
# 入口脚本以 root 修正 /data 归属后降权到 app(10001) 运行，兼容已有的宿主机目录。
RUN addgroup -S -g 10001 app && \
    adduser -S -G app -u 10001 -h /app app && \
    mkdir -p /data && \
    chown -R app:app /app /data && \
    chmod +x /usr/local/bin/docker-entrypoint.sh

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]

ENV OSS_HOST=0.0.0.0:8080 \
    OSS_DATA_DIR=/data \
    OSS_STORAGE_BACKEND=redb \
    OSS_DB_URL=redb:///data/oss.db \
    RUST_LOG=info

EXPOSE 8080
VOLUME ["/data"]

# busybox wget 用于健康检查。
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD wget -q -O - http://127.0.0.1:8080/healthz || exit 1

CMD ["/app/oss-server"]
