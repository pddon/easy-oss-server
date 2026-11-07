#!/usr/bin/env bash
# 从备份恢复数据目录与 MySQL 元数据库。
#
# 用法：./restore.sh <备份目录> [--yes]
#
# 安全策略：
#   - 恢复前把现有数据目录重命名为 data.bak-<时间戳>，不会直接覆盖。
#   - 恢复 MySQL 需要备份目录中存在 mysql.sql，且当前后端为 mysql。
#   - 恢复完成后启动服务并等待健康检查。

set -euo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

if [ $# -lt 1 ]; then
    die "用法：./restore.sh <备份目录> [--yes]"
fi

BACKUP_PATH="$1"
shift
ASSUME_YES="false"
while [ $# -gt 0 ]; do
    case "$1" in
        --yes|-y) ASSUME_YES="true"; shift ;;
        *) die "未知参数：$1" ;;
    esac
done

[ -d "$BACKUP_PATH" ] || die "备份目录不存在：$BACKUP_PATH"
[ -f "$BACKUP_PATH/data.tar.gz" ] || die "备份中缺少 data.tar.gz"

load_env
ensure_docker

if [ "$ASSUME_YES" != "true" ]; then
    warn "即将恢复备份：$BACKUP_PATH"
    warn "目标数据目录：$OSS_DATA_HOST_DIR（现有数据会重命名保留）"
    read -r -p "确认继续？输入 yes： " answer
    [ "$answer" = "yes" ] || die "已取消。"
fi

DATA_ABS="$(mkdir -p "$OSS_DATA_HOST_DIR" && cd "$OSS_DATA_HOST_DIR" && pwd)" \
    || die "无法访问数据目录 $OSS_DATA_HOST_DIR"
DATA_PARENT="$(dirname "$DATA_ABS")"
DATA_BASE="$(basename "$DATA_ABS")"

log "停止服务 ..."
compose stop "$OSS_CONTAINER_NAME" >/dev/null 2>&1 || true

# ---- MySQL 恢复 ----
if [ -f "$BACKUP_PATH/mysql.sql" ]; then
    if [ "${OSS_STORAGE_BACKEND:-redb}" != "mysql" ]; then
        warn "备份包含 mysql.sql，但当前后端不是 mysql，跳过 SQL 恢复。"
    else
        log "恢复 MySQL 元数据库 ..."
        DSN="$OSS_DB_URL"
        REST="${DSN#*://}"
        CREDS="${REST%%@*}"
        HOSTPART="${REST#*@}"
        MYSQL_USER="${CREDS%%:*}"
        MYSQL_PASS="${CREDS#*:}"
        MYSQL_PASS="$(printf '%s' "$MYSQL_PASS" | sed -e 's/%40/@/g; s/%3A/:/g; s/%3a/:/g; s/%2F/\//g; s/%2f/\//g')"
        MYSQL_DB="${HOSTPART#*/}"
        HOSTPORT="${HOSTPART%%/*}"
        MYSQL_HOST="${HOSTPORT%%:*}"
        MYSQL_PORT="${HOSTPORT#*:}"
        if [ "$MYSQL_PORT" = "$MYSQL_HOST" ]; then MYSQL_PORT="3306"; fi

        MYSQL_CONTAINER_ID="$(compose ps -q mysql 2>/dev/null | head -n1 || true)"
        if [ -n "$MYSQL_CONTAINER_ID" ]; then
            MYSQL_ROOT_PASSWORD="${MYSQL_ROOT_PASSWORD:-rootpass}"
            docker exec -i -e MYSQL_PWD="$MYSQL_ROOT_PASSWORD" "$MYSQL_CONTAINER_ID" \
                mysql -uroot < "$BACKUP_PATH/mysql.sql" || die "MySQL 恢复失败（compose 内 mysql 容器）。"
        elif command -v mysql >/dev/null 2>&1; then
            MYSQL_PWD="$MYSQL_PASS" mysql -h "$MYSQL_HOST" -P "$MYSQL_PORT" -u "$MYSQL_USER" \
                < "$BACKUP_PATH/mysql.sql" || die "MySQL 恢复失败（本机 mysql 客户端）。"
        else
            die "未找到可用的 MySQL 客户端：请安装 mysql 客户端，或使用 compose 内置 mysql 服务。"
        fi
        log "MySQL 恢复完成。"
    fi
fi

# ---- 数据目录恢复 ----
STAMP="$(date +%Y%m%d-%H%M%S)"
if [ -d "$DATA_ABS" ] && [ -n "$(ls -A "$DATA_ABS" 2>/dev/null || true)" ]; then
    log "保留现有数据目录：${DATA_ABS}.bak-${STAMP}"
    mv "$DATA_ABS" "${DATA_ABS}.bak-${STAMP}" || die "无法重命名现有数据目录（服务是否仍在写？）。"
fi
mkdir -p "$DATA_ABS"

log "解压数据归档 ..."
tar xzf "$BACKUP_PATH/data.tar.gz" -C "$DATA_PARENT" || die "数据归档解压失败。"
[ -d "$DATA_ABS" ] || die "解压后未找到数据目录 $DATA_ABS（归档结构可能不匹配）。"

log "启动服务 ..."
compose up -d "$OSS_CONTAINER_NAME" >/dev/null
wait_health 120 || die "恢复后健康检查未通过，请查看 ./logs.sh。旧数据保留在 ${DATA_ABS}.bak-${STAMP}"

log "恢复完成。旧数据保留在 ${DATA_ABS}.bak-${STAMP}（确认无误后可手动删除）。"
