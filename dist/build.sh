#!/usr/bin/env bash
# 构建 OSS Server 镜像。
#
# 用法：
#   ./build.sh [-v VERSION] [-i IMAGE] [-f FEATURES] [--no-cache] [--push]
#
# 示例：
#   ./build.sh -v 1.2.0
#   ./build.sh -v 1.2.0 -f mysql --push
#   ./build.sh -i registry.example.com/team/oss-server -v 1.2.0

set -euo pipefail

# shellcheck source=lib/common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

IMAGE="oss-server"
VERSION=""
FEATURES=""
NO_CACHE=""
PUSH="false"

while [ $# -gt 0 ]; do
    case "$1" in
        -v|--version) VERSION="$2"; shift 2 ;;
        -i|--image)   IMAGE="$2";   shift 2 ;;
        -f|--features) FEATURES="$2"; shift 2 ;;
        --no-cache)   NO_CACHE="--no-cache"; shift ;;
        --push)       PUSH="true"; shift ;;
        -h|--help)    sed -n '2,12p' "$0"; exit 0 ;;
        *) die "未知参数：$1（使用 --help 查看用法）" ;;
    esac
done

ensure_docker

if [ -z "$VERSION" ]; then
    VERSION="$(grep -m1 '^version' "$DIST_DIR/../Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
    VERSION="${VERSION:-0.0.0}"
fi

TAG="${IMAGE}:${VERSION}"
LATEST="${IMAGE}:latest"

log "构建镜像：$TAG（同时打标 $LATEST）"
log "构建特性：${FEATURES:-<默认 redb>}"

docker build $NO_CACHE \
    -f "$DIST_DIR/Dockerfile" \
    --build-arg CARGO_FEATURES="$FEATURES" \
    -t "$TAG" \
    -t "$LATEST" \
    "$DIST_DIR/.."

log "构建完成：$TAG"

if [ "$PUSH" = "true" ]; then
    log "推送镜像 ..."
    docker push "$TAG"
    docker push "$LATEST"
    log "推送完成。"
fi

cat <<EOF

下一步：
  1) 在 .env 中设置 OSS_IMAGE=${TAG}
  2) 执行 ./deploy.sh --no-build    （或直接 ./deploy.sh）
EOF
