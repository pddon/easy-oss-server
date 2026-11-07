#!/usr/bin/env bash
# TypeScript 客户端 npm 发布：build + dist 入口校验 + 内容白名单 + publish 预演。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
load_config
parse_args "$@"

PKG_DIR="$ROOT/clients/typescript"
NODE="${NODE:-node}"
NPM="${NPM:-npm}"
PKG_NAME="$("$NODE" -p "require('$PKG_DIR/package.json').name")"

heading "TypeScript 客户端（npm）：$PKG_NAME"

if [ -n "${NPM_TS_NAME:-}" ] && [ "$NPM_TS_NAME" != "$PKG_NAME" ]; then
  echo "[错误] release.env 的 NPM_TS_NAME=$NPM_TS_NAME 与 package.json 的 name=$PKG_NAME 不一致" >&2
  exit 1
fi
if [ "$DRY_RUN" -eq 0 ]; then
  require_var NPM_TOKEN "npm Automation token（scoped 包需要 publish 权限）"
fi

cd "$PKG_DIR"

echo "[1/5] 构建（dist/cjs + dist/esm + dist/types）"
"$NPM" run build

echo "[2/5] 类型检查"
"$NPM" run typecheck

echo "[3/5] 单元测试"
"$NPM" run test:unit

echo "[4/5] 发布内容校验（npm pack --dry-run）"
if [ "$DRY_RUN" -eq 0 ]; then export RELEASE_STRICT=1; fi
"$NODE" "$ROOT/scripts/release/check-npm-pack.js" "$PKG_DIR" dist

echo "[5/5] 发布预演（npm publish --dry-run）"
if [ "$DRY_RUN" -eq 0 ]; then
  "$NPM" publish --access public --//registry.npmjs.org/:_authToken="$NPM_TOKEN"
  finish "已发布 $PKG_NAME 到 npm"
else
  "$NPM" publish --dry-run --access public
  finish "dry-run 完成；确认 scope 与包名可用后加 --execute 真正发布"
fi
