#!/usr/bin/env bash
# 查看服务日志。
#
# 用法：./logs.sh [-f] [--tail N]
set -euo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

FOLLOW=""
TAIL="200"
while [ $# -gt 0 ]; do
    case "$1" in
        -f|--follow) FOLLOW="--follow"; shift ;;
        --tail)      TAIL="$2"; shift 2 ;;
        -h|--help)   sed -n '2,5p' "$0"; exit 0 ;;
        *) die "未知参数：$1" ;;
    esac
done

load_env
ensure_docker

compose logs $FOLLOW --tail="$TAIL" "$OSS_CONTAINER_NAME"
