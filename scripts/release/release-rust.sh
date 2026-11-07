#!/usr/bin/env bash
# crates.io 发布：cargo package / cargo publish --dry-run，默认只做本地校验。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
load_config
parse_args "$@"

CRATE_DIR="$ROOT/clients/rust"
CARGO="${CARGO:-cargo}"
REGISTRY="${CARGO_REGISTRY:-crates-io}"
NAME_IN_MANIFEST="$(sed -n 's/^name = "\(.*\)"$/\1/p' "$CRATE_DIR/Cargo.toml" | head -n 1)"
VERSION_IN_MANIFEST="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$CRATE_DIR/Cargo.toml" | head -n 1)"

heading "Rust 客户端（crates.io）：$NAME_IN_MANIFEST v$VERSION_IN_MANIFEST"

if [ -n "${CRATE_NAME:-}" ] && [ "$CRATE_NAME" != "$NAME_IN_MANIFEST" ]; then
  echo "[错误] release.env 的 CRATE_NAME=$CRATE_NAME 与 Cargo.toml 的 name=$NAME_IN_MANIFEST 不一致" >&2
  echo "       请先运行 python scripts/release/apply-names.py 同步包名" >&2
  exit 1
fi

if [ "$DRY_RUN" -eq 0 ]; then
  require_var CARGO_REGISTRY_TOKEN "crates.io API token（https://crates.io/settings/tokens）"
  if grep -n '__[A-Z_]*__' "$CRATE_DIR/Cargo.toml" >/dev/null 2>&1; then
    echo "[错误] Cargo.toml 仍有未替换的占位符：" >&2
    grep -n '__[A-Z_]*__' "$CRATE_DIR/Cargo.toml" >&2
    echo "       请先运行 python scripts/release/apply-names.py --apply" >&2
    exit 1
  fi
fi

cd "$CRATE_DIR"

echo "[1/4] 单元测试"
"$CARGO" test --lib --test sigv4_vectors --test client_behavior

echo "[2/4] 发布内容清单（cargo package --list）"
"$CARGO" package --list --allow-dirty

echo "[3/4] 打包校验（cargo package）"
"$CARGO" package --allow-dirty

echo "[4/4] 发布预演（cargo publish --dry-run）"
"$CARGO" publish --dry-run --allow-dirty --registry "$REGISTRY"

if [ "$DRY_RUN" -eq 0 ]; then
  echo "[execute] cargo publish"
  "$CARGO" publish --registry "$REGISTRY"
  finish "已发布 $NAME_IN_MANIFEST v$VERSION_IN_MANIFEST 到 crates.io"
else
  finish "dry-run 完成；确认无误后加 --execute 真正发布"
fi
