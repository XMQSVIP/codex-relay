#!/bin/sh
# Foreground entry point for shells and service managers; preserve signals and exit status.
set -eu
relay_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$relay_root"
if [ ! -x ./codex-relay ] || [ ! -f web/dist/index.html ]; then
    echo '请完整解压 Linux 发行包，保留 codex-relay 的执行权限和 web/dist。' >&2
    exit 1
fi
exec ./codex-relay serve "$@"
