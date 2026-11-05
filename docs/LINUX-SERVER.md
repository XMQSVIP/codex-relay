# Linux 服务器部署

Linux 使用无桌面模式：API Key 账户直接连接上游；本机配置和 ChatGPT/OAuth 账户通过独立 Codex 的 `app-server` 运行。不需要图形桌面，桌面自动检测、桌面调试和共享桌面连接不适用于 Linux。

## 运行要求

- x86_64（下载 `linux-x64`）或 aarch64（下载 `linux-arm64`），用 `uname -m` 确认。
- 发行程序使用 musl 静态链接，不要求主机安装特定版本的 glibc、Rust、Node.js 或 pnpm。适用于 Alibaba Cloud Linux 4 等 Linux 服务器；独立 Codex 自身的系统要求需另行满足。
- 原生账户需要支持 pidfd 的 Linux 内核（5.3 或以上）、可读的 `/proc` 和 `/bin/ps`（`procps` / `procps-ng`）；用进程句柄和创建时间跟踪跨会话的工具子进程。联网账户需可访问对应上游。
- 安装目录必须可写。`data/` 保存账户、访问设置与数据库，`.cache/` 保存独立 Codex 安装配置等状态；这两个目录属于同一套运行实例，一次只运行一个服务。

Alibaba Cloud Linux 4 可检查环境；缺少工具时由管理员安装：

```sh
uname -m
getconf GNU_LIBC_VERSION
command -v ps unzip
sudo dnf install -y procps-ng unzip ca-certificates
```

## 下载和首次启动

从 [GitHub Actions](https://github.com/XMQSVIP/codex-relay/actions/workflows/portable.yml) 下载对应 Linux 架构的构建产物，或从 [Releases](https://github.com/XMQSVIP/codex-relay/releases) 下载 ZIP 和同名 `.sha256`。Actions 下载若包含外层归档，先解开外层。

```sh
sha256sum -c CodexRelay-版本-linux-x64-时间.zip.sha256
unzip CodexRelay-版本-linux-x64-时间.zip
cd CodexRelay-版本-linux-x64-时间
chmod +x codex-relay start-linux.sh
./start-linux.sh --require-admin-token
```

启动器保持前台运行，不打开浏览器，Ctrl+C 或 SIGTERM 会触发正常退出。根目录 `relay.config.json` 默认端口为 4317；可用 `./start-linux.sh --port 8080 --require-admin-token` 临时覆盖。发行包无需使用 `pnpm start`，包内没有开发工具或源码。

服务器首次启动建议保留 `--require-admin-token`，终端会打印带 `#token=...` 的管理链接。主端口监听所有 IPv4 网卡，项目默认开启外部访问；客户端 `/v1` 始终还需单独创建的 API Key。先通过 SSH 隧道进入管理台设置后台密码，再按需配置公网和 HTTPS：

```sh
# 在自己的电脑运行，替换实际 SSH 用户和地址。
ssh -N -L 4317:127.0.0.1:4317 用户@服务器地址
```

在本机浏览器打开服务打印的 `http://127.0.0.1:4317/#token=...`。阿里云安全组和服务器防火墙按实际访问方式放行；使用上述隧道时无需开放公网 4317。反向代理和独立 API 端口见仓库的 [网络接入文档](https://github.com/XMQSVIP/codex-relay/blob/main/docs/NETWORK-ACCESS.md)。

## 配置原生账户

仅使用 API Key 账户时，直接在管理台添加上游，无需安装或检测 Codex。

使用 ChatGPT/OAuth 或本机配置账户时，可在「系统设置 → 环境与诊断 → 原生运行环境」选择「托管安装」：

- 启动后检查官方稳定版，此后每天检查一次；检查失败一小时后重试。只有点击「安装并启用 Codex」或「下载并启用新版」才下载，不自动升级。
- 页面显示对应架构的官方 npm 下载链接并支持复制。下载源为 `registry.npmjs.org/@openai/codex`，下载后校验官方 SHA-512、解压结构、架构、实际版本和隔离 `app-server` 初始化。
- 更新检查和下载沿用服务进程的自动代理配置：Linux 使用 `HTTPS_PROXY`、`HTTP_PROXY`、`ALL_PROXY` 和 `NO_PROXY` 环境变量；没有代理时直连，代理失败不回退直连。账户自定义代理只属于该账户，不影响下载。
- 所有版本保存在账户数据目录的同级 `native-codex/versions/` 中，当前选择仍保存在 `.cache/launch-config.json`。更新失败不会覆盖当前版本；下载过程可取消。
- 任意已安装旧版本均可重新校验并启用，也可手动删除。正在处理请求、维护或等待工具回调时不能切换；安装好的版本会保留，空闲后点击启用。已启用账户会按进程池规则启动新版本，已有进程不会被下载操作中断。
- 当前启用版本与仍被账户进程使用的版本禁止删除。先切换版本并停止仍使用旧版的账户实例，再删除旧版。程序不会自动清理旧版本。

已有二进制时选择「独立模式」，填写服务器上的 Codex 绝对路径即可校验并直接使用，不会触发下载安装。也可手动下载链接中的压缩包，解压后指定其中 `vendor/<架构>/bin/codex`（部分版本为 `codex/codex`）。命令行配置在运行目录中、以服务运行用户执行：

```sh
./codex-relay prepare --codex-path /绝对路径/codex
./start-linux.sh --require-admin-token
```

校验只运行版本查询和隔离初始化，不登录或调用模型。独立模式下手动更新 Codex 文件后重新校验路径。

添加独立账户后优先使用设备码登录，在自己的电脑打开验证网址完成授权；也可导入自己的有效凭据。浏览器 OAuth 回调在服务器的 `127.0.0.1:1455`，远程浏览器登录需另外转发本机 1455 到服务器 1455，或使用管理台已有的回调提交功能。本机配置账户读取服务运行用户的 Codex 配置；systemd 模板的 HOME 为 `/opt/codex-relay`。

## systemd 后台服务

发行包包含 `codex-relay.service`。以下命令用于首次安装到空的 `/opt/codex-relay`，需管理员权限；先结束前台验证实例。已有部署升级时先备份 `data/`、`.cache/` 和配置文件，停止服务后仅替换程序与前端文件，不覆盖运行数据。

```sh
# 在解压后的发行包目录中执行。
sudo useradd --system --user-group --home-dir /opt/codex-relay --shell /sbin/nologin codex-relay
sudo install -d -o codex-relay -g codex-relay /opt/codex-relay
sudo cp -a codex-relay start-linux.sh relay.config.json web codex-relay.service /opt/codex-relay/
sudo chown -R codex-relay:codex-relay /opt/codex-relay
sudo install -m 644 codex-relay.service /etc/systemd/system/codex-relay.service
sudo systemctl daemon-reload
sudo systemctl enable --now codex-relay
sudo journalctl -u codex-relay -n 30 --no-pager
```

用户已存在时跳过 `useradd`。模板自动重启异常退出的服务，手动停止不会重启。服务日志在 journal，管理令牌每次启动重新生成，从 journal 读取完整管理链接。`systemctl stop codex-relay` 先向主进程发送 SIGTERM，等待请求和原生进程清理；超过 90 秒由 systemd 回收剩余进程。

原生账户仍可从浏览器配置；命令行配置需在 `/opt/codex-relay` 执行 `sudo -u codex-relay env HOME=/opt/codex-relay ./codex-relay prepare --codex-path /绝对路径/codex`。程序及父目录应允许该用户读取和执行。

如需 HTTP 代理，可由管理员创建权限为 0600 的 `/etc/codex-relay.env`，填写 `HTTPS_PROXY`、`HTTP_PROXY` 和 `NO_PROXY=127.0.0.1,localhost` 后重启服务，也可使用管理台账户代理设置。独立 API 监听可通过 systemd override 清空原 `ExecStart=` 后重新指定启动命令，加入 `--network-config /opt/codex-relay/data/network.json`。

## 源码构建和检查

需要 Node.js 24、项目指定的 pnpm、稳定 Rust 和 C 编译器。Alibaba Cloud Linux 4 可用 `sudo dnf install -y gcc gcc-c++ make` 准备 C 工具链，再安装 Node.js 24、Rust 和 pnpm。普通源码构建使用本机 GNU target，无需 musl：

```sh
pnpm install --frozen-lockfile
pnpm build:release
sh scripts/Start-Relay.sh --require-admin-token
```

构建必须经过内存检查入口，小内存机器限制为单任务。也可在其他机器构建静态发行包后上传，无需在业务服务器编译。

发行包在原生 Linux x64 / ARM64 构建机生成。Ubuntu 构建机安装 `musl-tools` 并添加对应 Rust target，然后运行：

```sh
# x64 示例；ARM64 将 target 改为 aarch64-unknown-linux-musl。
rustup target add x86_64-unknown-linux-musl
export CC=musl-gcc
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc
python3 scripts/package_linux.py
```

ARM64 同时设置 `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc`。打包器检查 ELF 架构并拒绝动态解释器或动态库依赖，只复制发行文件，生成 SHA-256 和第三方许可证。解压验证覆盖含空格路径、无 Node 环境启动、管理鉴权、页面资源、重复实例拒绝、SIGTERM 和同目录重启，通过后才输出正式 ZIP。

定向检查：

```sh
pnpm check:quick:rust
python3 scripts/test_linux_package.py
node scripts/cargo-with-memory.mjs test --locked --test linux_platform --test admin --test network --test api_key_upstreams
node scripts/cargo-with-memory.mjs test --locked --lib installation::
node scripts/cargo-with-memory.mjs test --locked --lib process_tree::
```

原生初始化可单独验证：设置 `BRIDGE_NATIVE_BINARY=/绝对路径/codex` 后运行 `node scripts/cargo-with-memory.mjs test --locked --lib standalone_binary_initializes_without_desktop -- --ignored`。默认回归和发行包冒烟不等于真实 OAuth 登录、模型请求或容量验证。
