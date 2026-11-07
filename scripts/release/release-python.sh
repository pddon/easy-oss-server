#!/usr/bin/env bash
# PyPI 发布：构建 wheel + sdist，twine check，可选上传到 TestPyPI / PyPI。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
load_config
parse_args "$@"

PKG_DIR="$ROOT/clients/python"
PYTHON="${PYTHON:-python}"
TARGET="${PYPI_TARGET:-testpypi}"
PKG_NAME="$(sed -n 's/^name = "\([^"]*\)"$/\1/p' "$PKG_DIR/pyproject.toml" | head -n 1)"

heading "Python 客户端（PyPI，目标：$TARGET）：$PKG_NAME"

if [ -n "${PYPI_NAME:-}" ] && [ "$PYPI_NAME" != "$PKG_NAME" ]; then
  echo "[错误] release.env 的 PYPI_NAME=$PYPI_NAME 与 pyproject.toml 的 name=$PKG_NAME 不一致" >&2
  exit 1
fi

case "$TARGET" in
  testpypi)
    REPO_URL="https://test.pypi.org/legacy/"
    ;;
  pypi)
    REPO_URL="https://upload.pypi.org/legacy/"
    ;;
  *)
    echo "[错误] PYPI_TARGET 只能是 testpypi 或 pypi（当前：$TARGET）" >&2
    exit 1
    ;;
esac

if [ "$DRY_RUN" -eq 0 ]; then
  require_var TWINE_USERNAME "TestPyPI/PyPI 用户名（通常是 __token__）"
  require_var TWINE_PASSWORD "TestPyPI/PyPI API token"
  if grep -n '__[A-Z_]*__' "$PKG_DIR/pyproject.toml" >/dev/null 2>&1; then
    echo "[错误] pyproject.toml 仍有未替换的占位符：" >&2
    grep -n '__[A-Z_]*__' "$PKG_DIR/pyproject.toml" >&2
    echo "       请先运行 python scripts/release/apply-names.py --apply" >&2
    exit 1
  fi
fi

cd "$PKG_DIR"

echo "[1/4] 清理旧产物"
rm -rf build dist ./*.egg-info

echo "[2/4] 构建 wheel + sdist"
if "$PYTHON" -m build --version >/dev/null 2>&1; then
  "$PYTHON" -m build
else
  echo "[提示] 未安装 build 包（python -m build），仅构建 wheel；sdist 请先执行: python -m pip install build"
  "$PYTHON" -m pip wheel . --no-deps -w dist
fi

echo "[3/4] twine check"
if ! "$PYTHON" -m twine --version >/dev/null 2>&1; then
  echo "[错误] 缺少 twine，请先执行: python -m pip install twine" >&2
  exit 1
fi
"$PYTHON" -m twine check dist/*

echo "[4/4] 上传"
if [ "$DRY_RUN" -eq 0 ]; then
  "$PYTHON" -m twine upload --repository-url "$REPO_URL" dist/*
  finish "已上传到 $TARGET"
else
  echo "[dry-run] python -m twine upload --repository-url $REPO_URL dist/*"
  finish "dry-run 完成；先用 TestPyPI 验证，再加 --execute 正式上传"
fi
