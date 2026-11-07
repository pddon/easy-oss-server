#!/usr/bin/env bash
# 卸载服务。
#
# 用法：
#   ./uninstall.sh           停止并移除容器 / 网络（保留数据、镜像、备份）
#   ./uninstall.sh --purge   额外删除数据目录与本地镜像（不可恢复，需二次确认）
#
# 备份请先执行 ./backup.sh。

set -euo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

PURGE="false"
ASSUME_YES="false"
while [ $# -gt 0 ]; do
    case "$1" in
        --purge) PURGE="true"; shift ;;
        --yes|-y) ASSUME_YES="true"; shift ;;
        -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
        *) die "未知参数：$1" ;;
    esac
done

load_env
ensure_docker

if [ "$PURGE" != "true" ]; then
    log "移除容器与网络（数据目录与镜像保留）..."
    compose down --remove-orphans
    log "卸载完成。数据目录仍保留在：$OSS_DATA_HOST_DIR"
    log "如需彻底清理，执行：./uninstall.sh --purge"
    exit 0
fi

if [ "$ASSUME_YES" != "true" ]; then
    warn "危险操作：将删除数据目录 $OSS_DATA_HOST_DIR 和本地镜像 $OSS_IMAGE"
    read -r -p "确认请输入 DELETE： " answer
    [ "$answer" = "DELETE" ] || die "已取消。"
fi

log "移除容器与网络 ..."
compose down --remove-orphans || true

if [ -d "$OSS_DATA_HOST_DIR" ]; then
    log "删除数据目录：$OSS_DATA_HOST_DIR"
    rm -rf "$OSS_DATA_HOST_DIR" || die "删除数据目录失败（检查权限）。"
fi

log "删除本地镜像：$OSS_IMAGE"
docker rmi "$OSS_IMAGE" >/dev/null 2>&1 || warn "镜像删除失败或不存在：$OSS_IMAGE"

log "彻底卸载完成。备份目录（${BACKUP_DIR}）与会话密钥 .env 未被删除，如需清理请手动删除。"
