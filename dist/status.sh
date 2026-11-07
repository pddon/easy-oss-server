#!/usr/bin/env bash
# 查看服务状态：容器状态、健康状态、HTTP 健康检查、数据目录占用。
set -euo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

load_env
ensure_docker

echo "=== 容器状态 ==="
compose ps || true

echo
echo "=== 健康检查 ==="
HEALTH_URL="$(health_url "$OSS_PORT")"
echo "URL: $HEALTH_URL"
if RESP="$(http_get "$HEALTH_URL")"; then
    echo "$RESP"
else
    warn "健康检查未通过（服务可能未启动或仍在启动中）。"
fi

echo
echo "=== 最近一次启动自检 / 清理记录 ==="
if command -v curl >/dev/null 2>&1 && [ -n "${OSS_ADMIN_TOKEN:-}" ]; then
    curl -fsS --max-time 5 -H "X-Admin-Token: ${OSS_ADMIN_TOKEN}" \
        "$(health_url "$OSS_PORT" | sed 's#/healthz#/api/admin/maintenance/status#')" 2>/dev/null | \
        sed 's/},{/},\n{/g' | head -n 30 || warn "无法读取维护状态。"
else
    warn "缺少 curl 或 OSS_ADMIN_TOKEN，跳过维护状态查询。"
fi

echo
echo "=== 数据目录 ==="
if [ -d "$OSS_DATA_HOST_DIR" ]; then
    du -sh "$OSS_DATA_HOST_DIR" 2>/dev/null || true
else
    warn "数据目录不存在：$OSS_DATA_HOST_DIR"
fi
