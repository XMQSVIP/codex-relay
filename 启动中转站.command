#!/bin/zsh -f

# Finder runs .command files in Terminal with a minimal environment.
set -u
setopt pipefail
bridge_root=${0:A:h}
bridge_port=''
open_browser=1
launch_lock_fd=''

fail() {
    print -u2 -r -- "启动失败：$*"
    return 1
}

bridge_healthy() {
    local health
    health=$(/usr/bin/curl --noproxy '*' --connect-timeout 1 --max-time 2 \
        --fail --silent "$base_url/healthz") || return 1
    [[ "$(print -r -- "$health" | /usr/bin/plutil -extract status raw -o - - 2>/dev/null)" == ok \
        && "$(print -r -- "$health" | /usr/bin/plutil -extract implementation raw -o - - 2>/dev/null)" == rust ]]
}

bridge_listeners() {
    /usr/sbin/lsof -nP -tiTCP:"$bridge_port" -sTCP:LISTEN 2>/dev/null || true
}

listeners_belong_to_project() {
    local bridge_pid process_path process_root
    for bridge_pid in ${(f)1}; do
        process_path=$(/bin/ps -p "$bridge_pid" -o comm=) || return 1
        [[ "$process_path" == "$bridge_root/codex-relay" \
            || "$process_path" == "$bridge_root/target/debug/codex-relay" \
            || "$process_path" == "$bridge_root/target/release/codex-relay" ]] || return 1
        process_root=$(/usr/sbin/lsof -a -p "$bridge_pid" -d cwd -Fn 2>/dev/null \
            | /usr/bin/sed -n 's/^n//p') || return 1
        [[ "$process_root" == "$bridge_root" ]] || return 1
    done
}

main() {
    while (( $# )); do
        case "$1" in
            --port)
                (( $# >= 2 )) || { fail '--port 后需要端口号。'; return 1; }
                [[ "$2" == <-> && ${#2} -le 5 ]] || { fail '端口须为 1024–65535 的整数。'; return 1; }
                bridge_port=$(( 10#$2 ))
                (( bridge_port >= 1024 && bridge_port <= 65535 )) || { fail '端口须为 1024–65535 的整数。'; return 1; }
                shift 2
                ;;
            --no-browser) open_browser=0; shift ;;
            *) fail "未知参数：$1"; return 1 ;;
        esac
    done

    cd -- "$bridge_root" || return 1
    local entry
    entry="$bridge_root/codex-relay"
    if [[ ! -x "$entry" ]]; then
        entry="$bridge_root/target/release/codex-relay"
    fi
    if [[ ! -x "$entry" ]]; then
        entry="$bridge_root/target/debug/codex-relay"
    fi
    [[ -x "$entry" ]] \
        || { fail '缺少可执行程序，请先在项目目录执行 pnpm build。'; return 1; }
    local -a resolve_arguments=(serve --print-port)
    if [[ -n "$bridge_port" ]]; then
        resolve_arguments+=(--port "$bridge_port")
    fi
    bridge_port=$("$entry" "${resolve_arguments[@]}") || return 1
    local base_url="http://127.0.0.1:$bridge_port"
    local cache="$bridge_root/.cache"
    local stdout="$cache/launcher-$bridge_port.stdout.log"
    local stderr="$cache/launcher-$bridge_port.stderr.log"
    local listeners bridge_pid attempt ready=0

    /bin/mkdir -p -- "$cache" || { fail '无法创建 .cache 日志目录。'; return 1; }
    # Keep the lock file: removing it can let concurrent clicks lock different files.
    zmodload zsh/system || { fail '无法加载 macOS 启动锁。'; return 1; }
    : >> "$cache/launcher-$bridge_port.lock" || return 1
    zsystem flock -t 30 -f launch_lock_fd "$cache/launcher-$bridge_port.lock" \
        || { fail '另一启动器正在工作，请稍后重试。'; return 1; }

    listeners=$(bridge_listeners)
    if [[ -n "$listeners" ]]; then
        listeners_belong_to_project "$listeners" \
            || { fail "端口 $bridge_port 已被其他服务或另一份项目占用。"; return 1; }
        bridge_healthy || { fail "现有中转站尚未就绪，请检查 $stderr"; return 1; }
        print -r -- '中转站已在运行。'
    else
        [[ -f web/dist/index.html ]] \
            || { fail '缺少管理界面，请先在项目目录执行 pnpm build。'; return 1; }
        local -a bridge_arguments=(serve --port "$bridge_port")
        if [[ -f data/network.json ]]; then
            bridge_arguments+=(--network-config data/network.json)
        fi
        print -r -- '正在启动 Codex Relay…'
        /usr/bin/nohup "$entry" "${bridge_arguments[@]}" \
            </dev/null >"$stdout" 2>"$stderr" &!
        bridge_pid=$!
        print -r -- "$bridge_pid" > "$cache/launcher-$bridge_port.pid"
        for attempt in {1..100}; do
            /bin/kill -0 "$bridge_pid" 2>/dev/null \
                || { fail "服务已退出，请检查 $stderr"; return 1; }
            if bridge_healthy; then
                listeners=$(bridge_listeners)
                if [[ "${(f)listeners}" == "$bridge_pid" ]] \
                    && listeners_belong_to_project "$listeners"; then
                    ready=1
                    break
                fi
                fail "端口 $bridge_port 在启动期间被其他服务占用，请检查 $stderr"
                return 1
            fi
            /bin/sleep 0.25
        done
        (( ready )) || { fail "服务尚未就绪，请检查 $stdout 和 $stderr"; return 1; }
        print -r -- '中转站已在后台启动。'
    fi

    print -r -- "管理页面：$base_url/"
    print -r -- '可以关闭此终端窗口；停止服务请在管理页面点击“停止中转站”。'
    if (( open_browser )); then
        /usr/bin/open "$base_url/" || print -u2 -r -- '请手动打开上述管理页面。'
    fi
    return 0
}

trap 'if [[ -n "$launch_lock_fd" ]]; then zsystem flock -u "$launch_lock_fd"; fi' EXIT
main "$@"
launch_result=$?
if (( launch_result != 0 )) && [[ -t 0 ]]; then
    read -r '?按回车关闭此窗口…'
fi
exit "$launch_result"
