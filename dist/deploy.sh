#!/usr/bin/env bash
# 部署 / 启动 OSS Server（拉取或构建镜像 -> 启动 -> 健康检查）。
#
# 用法：
#   ./deploy.sh [--build] [--no-build] [-v VERSION] [-i IMAGE] [--timeout SECONDS]
#
#   --build        本地构建镜像后再启动
#   --no-build     不构建，直接使用 .env 中的 OSS_IMAGE（优先尝试 pull）
#   -v/-i          等价于 --build 并指定版本 / 镜像名

set -euo pipefail

# shellcheck source=lib/common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

BUILD="false"
TIMEOUT="120"
VERSION=""
IMAGE=""

while [ $# -gt 0 ]; do
    case "$1" in
        --build)    BUILD="true"; shift ;;
        --no-build) BUILD="false"; shift ;;
        -v|--version) VERSION="$2"; BUILD="true"; shift 2 ;;
        -i|--image)   IMAGE="$2";   shift 2 ;;
        --timeout)  TIMEOUT="$2"; shift 2 ;;
        -h|--help)  sed -n '2,10p' "$0"; exit 0 ;;
        *) die "未知参数：$1（使用 --help 查看用法）" ;;
    esac
done

ensure_env_file
load_env
ensure_docker

if [ -n "$IMAGE" ]; then
    export OSS_IMAGE="$IMAGE"
fi

# 目录权限与端口检查（提前失败，避免 compose up 后才暴露）。
check_data_dir "$OSS_DATA_HOST_DIR"
check_port "$OSS_PORT"

log "校验 compose 配置 ..."
compose config >/dev/null || die "docker-compose.yml 配置校验失败。"

if [ "$BUILD" = "true" ]; then
    build_args=()
    if [ -n "$VERSION" ]; then build_args+=(-v "$VERSION"); fi
    if [ -n "$IMAGE" ]; then build_args+=(-i "$IMAGE"); fi
    "$DIST_DIR/build.sh" "${build_args[@]}"
else
    log "尝试拉取镜像 ${OSS_IMAGE}（本地已有镜像可忽略失败）..."
    compose pull --ignore-pull-failures 2>/dev/null || \
        compose pull 2>/dev/null || \
        warn "镜像拉取失败或当前 compose 不支持 pull，将使用本地镜像。"
fi

log "启动服务 ..."
compose up -d --remove-orphans

if ! wait_health "$TIMEOUT"; then
    die "服务启动失败或健康检查超时。请查看日志：./logs.sh"
fi

ADMIN_URL="$(health_url "$OSS_PORT" | sed 's#/healthz##')/admin/"
cat <<EOF

部署完成。
  健康检查 : $(health_url "$OSS_PORT")
  管理后台 : ${ADMIN_URL}
  数据目录 : ${OSS_DATA_HOST_DIR}
  查看日志 : ./logs.sh
  查看状态 : ./status.sh
EOF
