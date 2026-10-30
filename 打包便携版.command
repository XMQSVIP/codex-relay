#!/bin/zsh -f
set -u
setopt pipefail
bridge_root=${0:A:h}
# Finder does not inherit the developer terminal's PATH.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
bundled_node="$HOME/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin"
if ! command -v node >/dev/null 2>&1 && [[ -x "$bundled_node/node" ]]; then
    export PATH="$bundled_node:$PATH"
fi
if command -v python3 >/dev/null 2>&1; then
    cd -- "$bridge_root" && python3 scripts/package_macos.py "$@"
    package_result=$?
else
    print -u2 -r -- '打包需要 Python 3.9 或以上版本。接收便携包的人无需安装 Python。'
    package_result=1
fi
if (( package_result != 0 )); then
    print -u2 -r -- '便携包未生成完成，请检查上面的错误。'
fi
if [[ -t 0 ]]; then
    read -r '?按回车关闭此窗口…'
fi
exit "$package_result"
