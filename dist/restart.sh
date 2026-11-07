#!/usr/bin/env bash
# 重启服务并等待健康检查。
#
# 用法：./restart.sh [--timeout SECONDS]
set -euo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

TIMEOUT="120"
while [ $# -gt 0 ]; do
    case "$1" in
        --timeout) TIMEOUT="$2"; shift 2 ;;
        -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
        *) die "未知参数：$1" ;;
    esac
done

load_env
ensure_docker

log "重启服务 ${OSS_CONTAINER_NAME} ..."
compose restart "$OSS_CONTAINER_NAME" >/dev/null
wait_health "$TIMEOUT" || die "重启后健康检查未通过，请执行 ./logs.sh 查看日志。"
log "重启完成。"
