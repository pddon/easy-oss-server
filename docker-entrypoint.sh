#!/bin/sh
# 容器入口：以 root 启动时修正 /data 归属，然后降权到 app(10001) 运行。
# 以非 root 启动（docker run --user）时不做 chown，直接运行。
set -eu

if [ "$(id -u)" = "0" ]; then
  mkdir -p /data
  chown -R app:app /data 2>/dev/null || true
  exec su-exec app:app "$@"
fi

exec "$@"
