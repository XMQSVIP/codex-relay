#!/bin/sh
set -eu
bridge_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$bridge_root"
echo '保持官方 Codex 桌面运行；这里只检测本机安装，不发送模型请求。'
if [ -x ./codex-relay ]; then
    exec ./codex-relay prepare "$@"
fi
if [ -x target/release/codex-relay ]; then
    exec target/release/codex-relay prepare "$@"
fi
if [ -x target/debug/codex-relay ]; then
    exec target/debug/codex-relay prepare "$@"
fi
exec node "$bridge_root/scripts/cargo-with-memory.mjs" run --locked -- prepare "$@"
