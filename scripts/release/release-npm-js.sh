#!/usr/bin/env bash
# JavaScript 客户端 npm 发布：build + 文件清单校验 + publish 预演。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
load_config
parse_args "$@"

PKG_DIR="$ROOT/clients/js"
NODE="${NODE:-node}"
NPM="${NPM:-npm}"

heading "JavaScript 客户端（npm）：$("$NODE" -p "require('$PKG_DIR/package.json').name")"

if [ "$DRY_RUN" -eq 0 ]; then
  require_var NPM_TOKEN "npm Automation token（https://www.npmjs.com/settings/<user>/tokens）"
fi

cd "$PKG_DIR"

echo "[1/4] 构建自检"
"$NPM" run build

echo "[2/4] 单元测试"
"$NPM" run test:unit

echo "[3/4] 发布内容校验（npm pack --dry-run）"
if [ "$DRY_RUN" -eq 0 ]; then export RELEASE_STRICT=1; fi
"$NODE" "$ROOT/scripts/release/check-npm-pack.js" "$PKG_DIR" index.js index.mjs lib types

echo "[4/4] 发布预演（npm publish --dry-run）"
if [ "$DRY_RUN" -eq 0 ]; then
  "$NPM" publish --access public --//registry.npmjs.org/:_authToken="$NPM_TOKEN"
  finish "已发布到 npm"
else
  "$NPM" publish --dry-run --access public
  finish "dry-run 完成；确认包名未被占用后加 --execute 真正发布"
fi
