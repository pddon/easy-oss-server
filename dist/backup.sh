#!/usr/bin/env bash
# 备份数据目录（对象文件 + redb 元数据库）以及 MySQL 元数据库（若使用）。
#
# 用法：./backup.sh [--out DIR]
#
# 产物：
#   backups/oss-backup-YYYYmmdd-HHMMSS/
#     data.tar.gz     数据目录归档
#     mysql.sql       MySQL 逻辑备份（仅 mysql 后端）
#     manifest.txt    备份信息
#
# 为保证 redb 数据一致性，备份期间会短暂停止服务，完成后自动恢复原运行状态。

set -euo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/common.sh"

while [ $# -gt 0 ]; do
    case "$1" in
        --out) BACKUP_DIR="$2"; shift 2 ;;
        -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
        *) die "未知参数：$1" ;;
    esac
done

load_env
ensure_docker

TS="$(date +%Y%m%d-%H%M%S)"
OUT="${BACKUP_DIR%/}/oss-backup-${TS}"
mkdir -p "$OUT" || die "无法创建备份目录 $OUT（检查 BACKUP_DIR 与权限）。"

DATA_ABS="$(cd "$DIST_DIR" && cd "$OSS_DATA_HOST_DIR" && pwd)" || die "数据目录不存在：$OSS_DATA_HOST_DIR"

WAS_RUNNING="false"
if compose ps -q "$OSS_CONTAINER_NAME" 2>/dev/null | grep -q .; then
    WAS_RUNNING="true"
fi

restore_running() {
    if [ "$WAS_RUNNING" = "true" ]; then
        log "恢复服务运行状态 ..."
        compose start "$OSS_CONTAINER_NAME" >/dev/null 2>&1 || true
    fi
}
trap restore_running EXIT

# ---- MySQL 逻辑备份（先做，mysqldump 自带一致性快照） ----
if [ "${OSS_STORAGE_BACKEND:-redb}" = "mysql" ]; then
    DSN="$OSS_DB_URL"
    REST="${DSN#*://}"
    CREDS="${REST%%@*}"
    HOSTPART="${REST#*@}"
    MYSQL_USER="${CREDS%%:*}"
    MYSQL_PASS="${CREDS#*:}"
    MYSQL_PASS="$(printf '%s' "$MYSQL_PASS" | sed -e 's/%40/@/g; s/%3A/:/g; s/%3a/:/g; s/%2F/\//g; s/%2f/\//g')"
    HOSTDB="${HOSTPART}"
    MYSQL_DB="${HOSTDB#*/}"
    HOSTPORT="${HOSTDB%%/*}"
    MYSQL_HOST="${HOSTPORT%%:*}"
    MYSQL_PORT="${HOSTPORT#*:}"
    if [ "$MYSQL_PORT" = "$MYSQL_HOST" ]; then MYSQL_PORT="3306"; fi

    log "备份 MySQL 元数据库 ${MYSQL_HOST}:${MYSQL_PORT}/${MYSQL_DB} ..."
    MYSQL_CONTAINER_ID="$(compose ps -q mysql 2>/dev/null | head -n1 || true)"
    if [ -n "$MYSQL_CONTAINER_ID" ]; then
        MYSQL_ROOT_PASSWORD="${MYSQL_ROOT_PASSWORD:-rootpass}"
        docker exec -e MYSQL_PWD="$MYSQL_ROOT_PASSWORD" "$MYSQL_CONTAINER_ID" \
            mysqldump -uroot --single-transaction --routines --databases "$MYSQL_DB" \
            > "$OUT/mysql.sql" || die "MySQL 备份失败（compose 内 mysql 容器）。"
    elif command -v mysqldump >/dev/null 2>&1; then
        MYSQL_PWD="$MYSQL_PASS" mysqldump -h "$MYSQL_HOST" -P "$MYSQL_PORT" -u "$MYSQL_USER" \
            --single-transaction --routines "$MYSQL_DB" > "$OUT/mysql.sql" \
            || die "MySQL 备份失败（本机 mysqldump）。"
    else
        docker run --rm --network host -e MYSQL_PWD="$MYSQL_PASS" \
            "${MYSQL_BACKUP_IMAGE:-mysql:8.0}" \
            mysqldump -h "$MYSQL_HOST" -P "$MYSQL_PORT" -u "$MYSQL_USER" \
            --single-transaction --routines "$MYSQL_DB" > "$OUT/mysql.sql" \
            || die "MySQL 备份失败（mysql 容器）。"
    fi
    log "MySQL 备份完成：$OUT/mysql.sql"
fi

# ---- 数据目录归档 ----
log "停止服务以保证元数据一致 ..."
compose stop "$OSS_CONTAINER_NAME" >/dev/null 2>&1 || true

log "归档数据目录 $DATA_ABS ..."
DATA_PARENT="$(dirname "$DATA_ABS")"
DATA_BASE="$(basename "$DATA_ABS")"
tar czf "$OUT/data.tar.gz" -C "$DATA_PARENT" "$DATA_BASE" \
    || die "数据目录归档失败（检查 tar 与目录权限）。"

{
    echo "created_at=${TS}"
    echo "image=${OSS_IMAGE}"
    echo "backend=${OSS_STORAGE_BACKEND}"
    echo "db_url=$(printf '%s' "$OSS_DB_URL" | sed -E 's#(://[^:/@]+):[^@]*@#\1:***@#')"
    echo "data_dir=${DATA_ABS}"
    echo "trash_retention_days=${OSS_TRASH_RETENTION_DAYS:-30}"
    echo "cleanup_interval_minutes=${OSS_CLEANUP_INTERVAL_MINUTES:-30}"
    echo "data_archive=data.tar.gz"
    if [ -f "$OUT/mysql.sql" ]; then echo "mysql_dump=mysql.sql"; fi
} > "$OUT/manifest.txt"

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$OUT" && sha256sum data.tar.gz mysql.sql 2>/dev/null > SHA256SUMS || true)
fi

log "备份完成：$OUT"
du -sh "$OUT" 2>/dev/null || true
