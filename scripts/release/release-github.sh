#!/usr/bin/env bash
# GitHub：打 annotate tag + 创建 Release。默认只打印命令，不执行任何远端写操作。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
load_config
parse_args "$@"

SERVER_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
VERSION="${VERSION:-$SERVER_VERSION}"
TAG="v$VERSION"
TITLE="${RELEASE_TITLE:-OSS Server $VERSION}"
NOTES_FILE="${RELEASE_NOTES_FILE:-}"

heading "GitHub Release：$GITHUB_ORG/$GITHUB_REPO tag=$TAG"

if [ -z "${GITHUB_REPO:-}" ]; then
  echo "[错误] 缺少 GITHUB_REPO" >&2
  exit 1
fi
if [ "$DRY_RUN" -eq 0 ]; then
  reject_placeholder GITHUB_ORG
fi

if ! command -v git >/dev/null 2>&1; then
  echo "[错误] 未找到 git" >&2
  exit 1
fi

cd "$ROOT"

if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
  echo "[错误] 工作区有未提交改动；打 tag 前请先提交或清理" >&2
  git status --short >&2
  exit 1
fi

if [ "$DRY_RUN" -eq 0 ]; then
  require_var GH_TOKEN "GitHub token（repo 权限；或改用已登录的 gh CLI）"
  if ! command -v gh >/dev/null 2>&1; then
    echo "[错误] 未找到 gh CLI，请安装后重试（GitHub Release 由 gh 创建）" >&2
    exit 1
  fi
  if ! git remote get-url origin >/dev/null 2>&1; then
    echo "[错误] 当前仓库没有 origin 远端，请先 git remote add origin <repo-url>" >&2
    exit 1
  fi
fi

NOTES_ARGS=(--generate-notes)
if [ -n "$NOTES_FILE" ]; then
  if [ ! -f "$NOTES_FILE" ]; then
    echo "[错误] RELEASE_NOTES_FILE 不存在：$NOTES_FILE" >&2
    exit 1
  fi
  NOTES_ARGS=(--notes-file "$NOTES_FILE")
fi

run git tag -a "$TAG" -m "OSS Server $VERSION"
run git push origin "$TAG"
run gh release create "$TAG" --title "$TITLE" "${NOTES_ARGS[@]}"

if [ "$DRY_RUN" -eq 0 ]; then
  finish "已创建 GitHub Release $TAG"
else
  finish "dry-run 完成：未执行任何 tag/push/release 操作"
fi
