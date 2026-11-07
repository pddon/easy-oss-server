#!/usr/bin/env bash
# 发布脚本公共函数。被其它 scripts/release/*.sh source。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# Git Bash / MSYS 下 pwd 返回 /e/... 形式，Node/npm 无法识别；改用 pwd -W 的 Windows 路径。
case "$(uname -s 2>/dev/null || echo unknown)" in
  MINGW*|MSYS*|CYGWIN*)
    ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -W)"
    ;;
esac
RELEASE_DIR="$ROOT/scripts/release"
CONFIG_FILE="$RELEASE_DIR/release.env"

# 加载配置：同名环境变量优先，配置文件只补空缺。
load_config() {
  [ -f "$CONFIG_FILE" ] || return 0
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      ''|'#'*) continue ;;
    esac
    key="${line%%=*}"
    value="${line#*=}"
    case "$key" in
      ''|*[!A-Za-z0-9_]*) continue ;;
    esac
    # shellcheck disable=SC2163
    if [ -z "${!key:-}" ]; then
      export "$key=$value"
    fi
  done < "$CONFIG_FILE"
}

DRY_RUN=1
FORCE=0

parse_args() {
  for arg in "$@"; do
    case "$arg" in
      --dry-run) DRY_RUN=1 ;;
      --execute|--publish) DRY_RUN=0 ;;
      --force) FORCE=1 ;;
      -h|--help)
        echo "用法: $0 [--dry-run|--execute] [--force]"
        echo "  默认 --dry-run：只做本地打包/校验，不上传、不推送。"
        echo "  --execute：真正发布（需要对应凭证）。"
        exit 0
        ;;
      *)
        echo "未知参数: $arg" >&2
        exit 2
        ;;
    esac
  done
}

require_var() {
  local name="$1" hint="${2:-}"
  if [ -z "${!name:-}" ]; then
    echo "[错误] 缺少环境变量或配置项 $name${hint:+（$hint）}" >&2
    exit 1
  fi
}

require_token() {
  local name="$1"
  if [ "$DRY_RUN" -ne 0 ]; then
    return 0
  fi
  require_var "$name" "真正发布时才需要；可先不加 --execute 做 dry-run"
}

reject_placeholder() {
  local name="$1" value="${!name:-}"
  case "$value" in
    *__*)
      echo "[错误] $name 仍是占位符（$value），请先在 scripts/release/release.env 或环境变量中替换" >&2
      exit 1
      ;;
  esac
}

run() {
  if [ "$DRY_RUN" -ne 0 ]; then
    printf '[dry-run] %s\n' "$*"
  else
    printf '[execute] %s\n' "$*"
    "$@"
  fi
}

heading() {
  echo
  echo "============================================================"
  echo "$1"
  echo "============================================================"
}

finish() {
  echo
  echo "[完成] $1"
}
