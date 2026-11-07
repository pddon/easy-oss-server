# Build stage
FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev openssl-dev perl make

WORKDIR /app

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.toml
COPY migrations migrations

# Create a dummy main.rs to build dependencies
RUN mkdir -p src && echo "fn main() {}" > src/main.rs
RUN cargo build --release 2>/dev/null || true

# Copy actual source
COPY src src
COPY migrations migrations

# 可选：--build-arg CARGO_FEATURES=mysql 启用 MySQL 后端（默认留空，纯 Rust 零 C 依赖）
ARG CARGO_FEATURES=""
RUN if [ -n "$CARGO_FEATURES" ]; then cargo build --release --features "$CARGO_FEATURES"; else cargo build --release; fi

# Runtime stage
FROM alpine:latest

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

# 非 root 运行；数据目录归 app 用户所有。
RUN addgroup -S -g 10001 app && \
    adduser -S -G app -u 10001 -h /app app && \
    mkdir -p /data && \
    chown -R app:app /app /data && \
    chmod +x /usr/local/bin/docker-entrypoint.sh

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]

ENV OSS_HOST=0.0.0.0:8080
ENV OSS_DATA_DIR=/data
ENV OSS_STORAGE_BACKEND=redb
ENV OSS_DB_URL=redb:///data/oss.db
# OSS_ADMIN_TOKEN 必须由使用者显式提供，镜像内不设默认值。
ENV OSS_PUBLIC_BASE_URL=http://localhost:8080
ENV OSS_SIGNATURE_EXPIRE_SECONDS=900
ENV OSS_MAX_UPLOAD_SIZE=10737418240
ENV OSS_CACHE_SIZE_BYTES=67108864
ENV RUST_LOG=info

EXPOSE 8080

VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD wget -q -O - http://127.0.0.1:8080/healthz || exit 1

CMD ["/app/oss-server"]
