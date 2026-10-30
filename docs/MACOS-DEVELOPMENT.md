# macOS 开发与启动

项目使用 Rust + React + SQLite，原生账户通过官方 app-server 执行。macOS 开发面向 Apple Silicon，验证入口与边界见下方[验证命令](#验证命令)。一键生成可分享 ZIP 的流程与独立解压验证见 [macOS 便携版](MACOS-PORTABLE.md)。

## 环境

需要 Xcode Command Line Tools、Rust stable（含 rustfmt / Clippy）、Node.js 24 和 pnpm 10.30.3。原生账户使用官方 Codex：自动模式检测正在运行的桌面，独立模式填写支持 `app-server` 的 Codex 可执行文件路径，无需桌面。仅使用 API Key 直连账户时无需准备原生安装。

探测依据运行中桌面的真实可执行路径和子进程，支持路径包含空格，不硬编码 `/Applications/Codex.app`。原生安装检测、账户接入检查和显式诊断会执行安装校验。客户端 HTTP/原生 WebSocket 推理使用已准备的安装信息，每次请求不再重新计算程序摘要、检查发布者签名或扫描归档。配置读取、原生进程启动、账户路由与会话策略检查仍正常执行。升级桌面后重新 prepare，并刷新账户接入信息；能力诊断是可选项，不是请求前置条件。

## 首次启动

在项目根目录执行：

```sh
pnpm install --frozen-lockfile
pnpm build:release
# 自动模式先启动官方桌面再检测；独立模式在服务启动后通过管理台设置
pnpm run prepare:desktop
pnpm start
```

管理界面为 `http://127.0.0.1:4317/`。已有构建可在 Finder 中双击项目根目录的 `启动中转站.command`：服务在后台启动，就绪后自动打开管理页面，重复双击会复用本项目的现有服务。可以关闭启动器的终端窗口和浏览器；停止服务请在管理页面点击“停止中转站”。日志位于 `.cache/launcher-4317.stdout.log` 和 `.cache/launcher-4317.stderr.log`。若端口被其他服务或另一份项目占用，启动器会提示错误。

启动器优先使用便携包根目录的 `codex-relay`，源码目录中则优先使用已构建的 `target/release/codex-relay`，不存在时尝试 `target/debug/codex-relay`，不需要 Rust、Node 或 pnpm 在 PATH 中。`pnpm build:release` 构建 release，`pnpm start` 使用 release；开发调试可运行 `pnpm start:debug`。切换构建版本前，先在管理页面停止已有服务，再启动。若存在 `data/network.json`，双击启动器会自动加载；源码项目首次仍需完成上述构建和原生安装准备。使用模型前须启用账户接入。

终端中也可执行 `./启动中转站.command --port 4327 --no-browser`，指定端口或跳过打开浏览器。若文件复制后丢失执行权限，在项目目录运行 `chmod +x 启动中转站.command`。

需要前台运行或重新检测原生安装时，使用以下脚本：

```sh
./scripts/Prepare-Relay.sh
./scripts/Start-Relay.sh
# 需要管理令牌时：
./scripts/Start-Relay.sh --require-admin-token
```

终端 Ctrl+C、SIGTERM 和页面“停止服务”均进入清理路径；关闭浏览器不会停止服务。开发前端可另开终端运行 `pnpm dev:web`。

## OAuth 与网络

1. 在“账户管理”点击“添加账户”，选择“使用本机 Codex”；检测成功后确认名称和分组，点击“连接并启用”。独立模式先在“系统设置 → 环境与诊断 → 原生运行环境”校验并保存 Codex 路径。
2. 在账户列表或详情检查“启用账户”状态；首次启用只读取认证与配置并预热实例，不发送模型推理请求。“测试连接”为可选诊断，会消耗用量，测试模型不会限制客户端模型选择。
3. 给账户所在分组创建客户端 API Key，使用 `http://127.0.0.1:4317/v1` 或 `ws://127.0.0.1:4317/v1/native`。

桥仅调用官方 `getAuthStatus(includeToken=true, refreshToken=false)` 读取桌面 AT，再通过 `account/login/start(chatgptAuthTokens)` 注入独立推理进程。绑定文件只保存身份、模式与代次，不保存桌面 AT/RT；刷新仍由原生后端负责。没有退出、改写或删除个人桌面登录。

若原生接口确认登录但没有返回 AT，显示 `DESKTOP_OAUTH_TOKEN_UNAVAILABLE`。浏览器/设备码独立登录、完整凭据导入及 AT-only 导入入口仍保留。账户根目录、实例和 home/workspace 为 `0700`，桥写入的凭据文件为 `0600`，拒绝符号链接账户目录。

可在“账户详情 → 接入与调度”配置账户的 app-server 代理。保存后立即重启原本运行的实例，会中断该账户当前请求和登录；停止的账户保持停止。选择“直连”清除子进程代理；选择“指定代理”使用填写的 HTTP/HTTPS 地址，详见 [账户代理](OPERATIONS.md#账户代理)。

默认“自动”模式下，独立 app-server 不自动读取 macOS 系统代理。桥会在没有显式 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` 环境变量时，将系统全局手动 HTTP/HTTPS 代理传给新原生进程；显式环境变量优先，本机回环始终绕过代理。PAC、自动发现和仅 SOCKS 的设置不会转换为 HTTP 代理；此时可显式提供官方后端支持的代理环境变量。

模型 HTTP/TLS、上游 WebSocket、认证续期和重试继续由 app-server 处理。macOS 通过 libproc 真实程序路径和 lsof TCP 四元组校验连接归属，合成测试保留此检查。

## 验证命令

日常按改动选择 `pnpm check:quick:rust` 或 `pnpm check:quick:web`，前后端都改才使用组合入口 `pnpm check:quick`，再运行相关模块回归；管理接口字段变化另须运行 `pnpm test:contracts`，纯文档只检查内容与链接。完整 `pnpm check` 用于发布前、明确要求全量验证或无法隔离影响的全栈基础设施变更，详见 [开发检查](DEVELOPMENT-CHECKS.md)。

原生安装、认证或生命周期改动可按影响选择以下 ignored 目标；这些测试需要本机已准备的官方安装，不属于日常默认检查：

```sh
export BRIDGE_NATIVE_INSTALLATION="$PWD/.cache/launch-config.json"
node scripts/cargo-with-memory.mjs test --locked -j1 \
  --test oauth_accounts --test native_direct --test native_responses \
  --test native_lifecycle --test native_cancellation \
  -- --ignored --test-threads=1
```

以上原生集成测试使用本机合成上游、合成凭据和临时账户目录，不消耗真实账号用量。生命周期测试还通过 `command/exec` 启动受控 sleep 后代，检查停止、Drop、父进程崩溃、Responses 取消及原生 turn/interrupt。

真实验收脚本须显式选择已登录账户；临时客户端 Key 只在脚本内存中使用，结束时撤销。脚本不启动浏览器登录，不打印或写出 OAuth 令牌：

```sh
node scripts/test_real_oauth.mjs --allow-real-requests \
  --account <桥内账户UUID> --verify-route
# 单独复验工具和多轮，避免重做已通过的文本请求：
node scripts/test_real_oauth.mjs --allow-real-requests \
  --account <桥内账户UUID> --skip-text
```

以下脚本只打开原生线程，不启动模型轮次；它会停止本机桥服务，用于验收断开、实例停止、服务退出和个人桌面进程保留。两次之间须重新启动桥：

```sh
node scripts/test_macos_lifecycle.mjs --account <桥内账户UUID> --shutdown api
./scripts/Start-Relay.sh
# 在另一个终端：
node scripts/test_macos_lifecycle.mjs --account <桥内账户UUID> --shutdown sigterm
```

安装缓存、原生协议与验收报告应在本机生成，不跨平台复制安装缓存或凭据。

## 边界

- 双真实账号隔离、过期续期、撤权和桌面切换账号需要对应真实环境验收；合成账号测试不能替代。
- 图片、搜索、生图和 namespace 扩展默认自动接入；账户可明确关闭图片、搜索或生图，无需先运行能力诊断。
- “桌面连接调试”仍为原有 Windows 调试流程；macOS 日常 OAuth/Responses/原生 WS 不依赖它，也不会重启个人桌面。
- macOS 使用新会话、持续跟踪后代的 PID/启动时间和退出时枚举回收，覆盖已测试的原生独立子会话；它不是面向恶意逃逸进程的操作系统容器。SIGKILL 直接杀死桥或系统崩溃无法运行正常退出清理，不作为本次保证。
