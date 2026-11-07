#!/usr/bin/env bash
# Docker 镜像：多架构（linux/amd64 + linux/arm64）构建；默认只构建 amd64，不推送。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
load_config
parse_args "$@"

SERVER_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
VERSION="${VERSION:-$SERVER_VERSION}"
BUILDER="${DOCKER_BUILDER:-oss-server-builder}"
PLATFORMS="${PLATFORMS:-linux/amd64,linux/arm64}"
CARGO_FEATURES="${CARGO_FEATURES:-}"

IMAGE_NAMESPACE="${DOCKERHUB_NAMESPACE:-}"
# dry-run 时占位符不阻断本地构建，用一个合法的本地命名空间替代。
if [ "$DRY_RUN" -ne 0 ]; then
  case "$IMAGE_NAMESPACE" in
    *__*) IMAGE_NAMESPACE="local" ;;
  esac
fi

heading "Docker 镜像：$IMAGE_NAMESPACE/oss-server:$VERSION"

if [ "$DRY_RUN" -eq 0 ]; then
  reject_placeholder DOCKERHUB_NAMESPACE
  require_var DOCKERHUB_USERNAME "Docker Hub 用户名"
  require_var DOCKERHUB_TOKEN "Docker Hub Access Token（需 push 权限）"
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "[错误] 未找到 docker，请先安装 Docker Engine / Docker Desktop" >&2
  exit 1
fi

IMAGE="$IMAGE_NAMESPACE/oss-server"
if docker buildx version >/dev/null 2>&1; then
  HAVE_BUILDX=1
else
  HAVE_BUILDX=0
fi

echo "[构建参数] platforms=$PLATFORMS features=${CARGO_FEATURES:-<none>} dockerfile=dist/Dockerfile"
echo "[产物标签] $IMAGE:$VERSION, $IMAGE:latest"

if [ "$DRY_RUN" -eq 0 ]; then
  if [ "$HAVE_BUILDX" -ne 1 ]; then
    echo "[错误] 正式多架构推送需要 docker buildx（docker buildx version 失败）" >&2
    exit 1
  fi

  echo "[1/3] docker login"
  printf '%s' "$DOCKERHUB_TOKEN" | docker login --username "$DOCKERHUB_USERNAME" --password-stdin

  echo "[2/3] 创建/复用 buildx builder"
  docker buildx inspect "$BUILDER" >/dev/null 2>&1 || docker buildx create --name "$BUILDER" --driver docker-container >/dev/null
  docker buildx use "$BUILDER"

  echo "[3/3] 多架构构建并推送"
  docker buildx build \
    --platform "$PLATFORMS" \
    --build-arg CARGO_FEATURES="$CARGO_FEATURES" \
    -f "$ROOT/dist/Dockerfile" \
    -t "$IMAGE:$VERSION" \
    -t "$IMAGE:latest" \
    --push \
    "$ROOT"
  finish "已推送 $IMAGE:$VERSION 与 latest（$PLATFORMS）"
else
  echo "[1/2] 本地构建（单架构，不推送）"
  if [ "$HAVE_BUILDX" -eq 1 ]; then
    docker buildx build \
      --platform linux/amd64 \
      --build-arg CARGO_FEATURES="$CARGO_FEATURES" \
      -f "$ROOT/dist/Dockerfile" \
      -t "$IMAGE:$VERSION" \
      --load \
      "$ROOT"
  else
    echo "[提示] 未检测到 buildx，回退到 docker build（仅本机架构）"
    docker build \
      --build-arg CARGO_FEATURES="$CARGO_FEATURES" \
      -f "$ROOT/dist/Dockerfile" \
      -t "$IMAGE:$VERSION" \
      "$ROOT"
  fi

  echo "[2/2] 本地运行冒烟（可选，按 Ctrl+C 结束）"
  echo "[dry-run] docker run --rm -e OSS_ADMIN_TOKEN=<token> -p 8080:8080 $IMAGE:$VERSION"
  finish "dry-run 完成；真实推送需要 --execute 且已 docker login"
fi
