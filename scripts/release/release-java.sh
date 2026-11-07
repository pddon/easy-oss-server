#!/usr/bin/env bash
# Java 客户端：构建并预演发布命令（Maven Central / GitHub Packages 二选一）。
# Maven 本身没有 --dry-run，所以默认只打包并打印 deploy 命令，不执行 deploy。
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
load_config
parse_args "$@"

JAVA_DIR="$ROOT/clients/java"
MVN="${MVN:-mvn}"
TARGET="${MAVEN_TARGET:-github-packages}"

GROUP_ID="$(sed -n 's:.*<groupId>\([^<]*\)</groupId>.*:\1:p' "$JAVA_DIR/pom.xml" | head -n 1)"
ARTIFACT_ID="$(sed -n 's:.*<artifactId>\([^<]*\)</artifactId>.*:\1:p' "$JAVA_DIR/pom.xml" | head -n 1)"

heading "Java 客户端（$TARGET）：$GROUP_ID:$ARTIFACT_ID"

if [ -n "${MAVEN_GROUP_ID:-}" ] && [ "$MAVEN_GROUP_ID" != "$GROUP_ID" ]; then
  echo "[错误] release.env 的 MAVEN_GROUP_ID=$MAVEN_GROUP_ID 与 pom.xml 的 groupId=$GROUP_ID 不一致" >&2
  echo "       请先运行 python scripts/release/apply-names.py --apply 同步包名" >&2
  exit 1
fi
if [ -n "${MAVEN_ARTIFACT_ID:-}" ] && [ "$MAVEN_ARTIFACT_ID" != "$ARTIFACT_ID" ]; then
  echo "[错误] release.env 的 MAVEN_ARTIFACT_ID=$MAVEN_ARTIFACT_ID 与 pom.xml 的 artifactId=$ARTIFACT_ID 不一致" >&2
  echo "       请先运行 python scripts/release/apply-names.py --apply 同步包名" >&2
  exit 1
fi

case "$TARGET" in
  github-packages|central) ;;
  *)
    echo "[错误] MAVEN_TARGET 只能是 github-packages 或 central（当前：$TARGET）" >&2
    exit 1
    ;;
esac

if [ "$TARGET" = "github-packages" ]; then
  if [ "$DRY_RUN" -eq 0 ]; then
    reject_placeholder GITHUB_ORG
    require_var GITHUB_TOKEN "GitHub Packages 发布用的 token（需 write:packages 权限）"
    require_var GITHUB_ACTOR "GitHub 用户名"
  fi
else
  if [ "$DRY_RUN" -eq 0 ]; then
    reject_placeholder MAVEN_CENTRAL_STAGING_URL
    echo "[提示] Central 发布需要 ~/.m2/settings.xml 中的 central 服务器凭证与 GPG 私钥" >&2
    require_var MAVEN_CENTRAL_USERNAME "Central 用户名（或改用 settings.xml server）"
  fi
fi

cd "$JAVA_DIR"

echo "[1/3] 构建（跳过测试）"
"$MVN" -B -DskipTests package

echo "[2/3] 源代码行数与产物"
ls -1 target/*.jar 2>/dev/null || true

echo "[3/3] 发布命令"
if [ "$DRY_RUN" -eq 0 ]; then
  "$MVN" -B deploy "-P$TARGET" -DskipTests
  finish "已发布到 $TARGET"
else
  echo "[dry-run] mvn -B deploy -P$TARGET -DskipTests"
  finish "dry-run 完成；Maven 没有真正的预演，建议先在本地仓库 mvn install 验证"
fi
