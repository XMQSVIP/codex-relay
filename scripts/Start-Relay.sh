#!/bin/sh
set -eu
bridge_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$bridge_root"
if [ ! -f web/dist/index.html ]; then
    echo '请先运行 pnpm build。' >&2
    exit 1
fi
if [ -x ./codex-relay ]; then
    exec ./codex-relay serve "$@"
fi
if [ -x target/release/codex-relay ]; then
    exec target/release/codex-relay serve "$@"
fi
if [ -x target/debug/codex-relay ]; then
    exec target/debug/codex-relay serve "$@"
fi
exec node "$bridge_root/scripts/cargo-with-memory.mjs" run --release --locked -- serve "$@"
