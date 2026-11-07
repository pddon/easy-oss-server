#!/usr/bin/env bash
# 公共函数：被 dist 下其他脚本 source。

set -euo pipefail

DIST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$DIST_DIR"

log()  { printf '\033[1;34m[INFO]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[WARN]\033[0m %s\n' "$*"; }
err()  { printf '\033[1;31m[ERROR]\033[0m %s\n' "$*" >&2; }

die() { err "$*"; exit 1; }

# 载入 .env（若存在）。
load_env() {
    if [ -f "$DIST_DIR/.env" ]; then
        set -a
        # shellcheck disable=SC1091
        . "$DIST_DIR/.env"
        set +a
    fi

    OSS_PORT="${OSS_PORT:-8080}"
    OSS_IMAGE="${OSS_IMAGE:-oss-server:latest}"
    OSS_CONTAINER_NAME="${OSS_CONTAINER_NAME:-oss-server}"
    OSS_DATA_HOST_DIR="${OSS_DATA_HOST_DIR:-./data}"
    OSS_STORAGE_BACKEND="${OSS_STORAGE_BACKEND:-redb}"
    OSS_DB_URL="${OSS_DB_URL:-redb:///data/oss.db}"
    BACKUP_DIR="${BACKUP_DIR:-./backups}"
}

ensure_env_file() {
    if [ ! -f "$DIST_DIR/.env" ]; then
        cp "$DIST_DIR/.env.example" "$DIST_DIR/.env"
        warn "未找到 .env，已从 .env.example 复制。请修改 OSS_ADMIN_TOKEN / OSS_MASTER_KEY 后重新执行。"
        exit 1
    fi
    if grep -q 'change-me-admin-token' "$DIST_DIR/.env" 2>/dev/null; then
        warn "检测到 OSS_ADMIN_TOKEN 仍为默认值，建议先修改 .env 再部署。"
    fi
}

# 优先 docker compose，其次旧版 docker-compose。
detect_compose() {
    if docker compose version >/dev/null 2>&1; then
        COMPOSE_KIND="plugin"
        return 0
    fi
    if command -v docker-compose >/dev/null 2>&1; then
        COMPOSE_KIND="standalone"
        return 0
    fi
    die "未检测到 docker compose（插件或 docker-compose）。请安装 Docker Compose 后重试：https://docs.docker.com/compose/install/"
}

compose() {
    if [ "${COMPOSE_KIND:-}" = "plugin" ]; then
        docker compose "$@"
    else
        docker-compose "$@"
    fi
}

# Docker Toolbox：daemon 在虚拟机内，需要先导出 docker-machine 环境。
ensure_docker() {
    command -v docker >/dev/null 2>&1 || die "未安装 docker，请先安装 Docker Engine / Docker Desktop。"

    if ! docker info >/dev/null 2>&1; then
        if command -v docker-machine >/dev/null 2>&1; then
            local machine="${DOCKER_MACHINE_NAME:-default}"
            warn "Docker daemon 未连接，尝试启动 docker-machine '${machine}' ..."
            docker-machine start "$machine" >/dev/null 2>&1 || true
            eval "$(docker-machine env --shell bash "$machine")"
            warn "已使用 docker-machine 环境：DOCKER_HOST=${DOCKER_HOST:-}"
        fi
    fi

    docker info >/dev/null 2>&1 || die "无法连接 Docker daemon，请确认 Docker 已启动（Docker Desktop 或 docker-machine）。"
    detect_compose
}

# 健康检查地址：Docker Toolbox 下 daemon 在虚拟机里，localhost 不可达。
health_url() {
    local port="$1"
    if [ -n "${DOCKER_HOST:-}" ]; then
        local host
        host="$(printf '%s' "$DOCKER_HOST" | sed -E 's#tcp://([^:]+):.*#\1#')"
        if [ "$host" != "$DOCKER_HOST" ] && [ "$host" != "127.0.0.1" ]; then
            printf 'http://%s:%s/healthz' "$host" "$port"
            return
        fi
    fi
    printf 'http://127.0.0.1:%s/healthz' "$port"
}

http_get() {
    local url="$1"
    if command -v curl >/dev/null 2>&1; then
        curl -fsS --max-time 3 "$url" 2>/dev/null
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O - --timeout=3 "$url" 2>/dev/null
    else
        return 1
    fi
}

# 等待健康检查通过；超时返回 1。
wait_health() {
    local timeout="${1:-120}"
    local url
    url="$(health_url "$OSS_PORT")"
    local waited=0
    log "等待健康检查通过：$url（最多 ${timeout}s）"
    while [ "$waited" -lt "$timeout" ]; do
        if http_get "$url" | grep -q '"status":"ok"'; then
            log "健康检查通过。"
            return 0
        fi
        sleep 2
        waited=$((waited + 2))
    done
    err "健康检查超时（${timeout}s）：$url"
    compose logs --tail=80 "$OSS_CONTAINER_NAME" 2>/dev/null || true
    return 1
}

# 端口占用检查：若端口已被非本服务占用则报错。
check_port() {
    local port="$1"
    local listeners=""
    if command -v ss >/dev/null 2>&1; then
        listeners="$(ss -ltn 2>/dev/null | awk -v p=":$port" '$4 ~ p {print $4}' || true)"
    elif command -v netstat >/dev/null 2>&1; then
        listeners="$(netstat -an 2>/dev/null | awk -v p=":$port " '$0 ~ p && $0 ~ /LISTEN/ {print $0}' || true)"
    fi

    if [ -n "$listeners" ]; then
        if docker ps --format '{{.Names}} {{.Ports}}' 2>/dev/null | grep -q "${OSS_CONTAINER_NAME}.*:${port}->"; then
            return 0
        fi
        die "宿主机端口 ${port} 已被占用。请修改 .env 中的 OSS_PORT，或停止占用该端口的进程后重试。"
    fi
}

# 数据目录权限检查。
check_data_dir() {
    local dir="${1:-$OSS_DATA_HOST_DIR}"
    mkdir -p "$dir" 2>/dev/null || die "无法创建数据目录 $dir（请检查路径与权限）。"
    local probe="$dir/.write-test.$$"
    : > "$probe" 2>/dev/null || die "数据目录 $dir 不可写（请检查挂载目录权限；Rootless Docker 下可执行 chown -R 10001:10001 $dir）。"
    rm -f "$probe"
    if [ ! -w "$dir" ] || [ ! -r "$dir" ]; then
        die "数据目录 $dir 缺少读写权限。"
    fi
}
