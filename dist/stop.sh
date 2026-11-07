#!/usr/bin/env bash
# 停止服务（保留容器与数据）。
set -euo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

load_env
ensure_docker

log "停止服务 ${OSS_CONTAINER_NAME} ..."
compose stop "$OSS_CONTAINER_NAME"
log "已停止。数据目录保持不变：$OSS_DATA_HOST_DIR"
