#!/bin/sh
set -eu
package_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$package_root"
if ! command -v python3 >/dev/null 2>&1; then
    echo 'Linux 打包需要 Python 3.12 或以上版本。' >&2
    exit 1
fi
exec python3 scripts/package_linux.py "$@"
