# 在 Windows 上继续开发

项目包含 Rust 后端、React 管理界面、测试、Windows/macOS 平台分支及依赖锁文件。在 Windows 本机安装依赖、构建并检测原生客户端；运行数据和安装缓存保存在本机，不随源码分发。

## 更新已经运行的 Windows 项目

项目名称为 Codex Relay（Codex 中转站），程序为 `codex-relay.exe`，源码目录为 `crates/relay/`。命名与打包约定见 [项目名称与图标](ARCHITECTURE.md#项目名称与图标)。以下步骤用于更新本机开发环境。

1. 在 Windows 管理页面正常停止中转站，备份现有源码、`data` 和 `.cache`。停止后再备份，避免复制正在写入的数据库。
2. 将新源码 ZIP 解压到临时目录，把其中 `codex-relay` 文件夹的内容复制到原项目根目录，替换同名源码。目标目录应直接包含 `Cargo.toml` 和 `package.json`，不要多嵌套一层 `codex-relay`。如果 Windows 上另有独立源码修改，先对比合并。
3. 保留 Windows 原来的 `.cache`、`target` 和 `node_modules`；本机 `data` 先备份，不从 Mac 复制运行数据。开发阶段存储结构调整后，测试数据可能需要重新初始化：停止服务，将 `data/accounts` 移出运行目录备份，再启动并重新配置账户和密钥。源码包不包含这些运行与构建目录。
4. 在 Windows 原项目目录的 PowerShell 中构建，每步失败就停止：

```powershell
cd C:\dev\codex-relay  # 改成现有项目目录
pnpm.cmd install --frozen-lockfile
if ($LASTEXITCODE -ne 0) { throw '依赖安装失败' }
pnpm.cmd run build:release
if ($LASTEXITCODE -ne 0) { throw 'Release 构建失败' }
```

如果根目录存在便携版的 `codex-relay.exe`，启动器会优先使用它。先备份并替换为刚编译的 Windows release 程序；没有这个文件则跳过：

```powershell
if (Test-Path -LiteralPath '.\codex-relay.exe') {
    $bridgeOldExe = '.\codex-relay.before-upgrade-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.exe'
    Copy-Item -LiteralPath '.\codex-relay.exe' -Destination $bridgeOldExe -ErrorAction Stop
    Copy-Item -LiteralPath '.\target\release\codex-relay.exe' -Destination '.\codex-relay.exe' -Force -ErrorAction Stop
}
.\启动中转站.cmd
```

启动器依次选择根目录便携程序、`target\release`、`target\debug`。管理后台默认仍为 `http://127.0.0.1:4317/`。启动时初始化空数据库或校验当前结构，校验失败会报错并保留数据；开发阶段不要求保留旧格式，见[开发阶段约定](RESPONSES-COMPATIBILITY.md#开发阶段约定)。若使用自定义端口或网络启动参数，沿用原参数。

源码更新后按 [开发检查](DEVELOPMENT-CHECKS.md) 选择对应侧的快速检查及相关回归。完整 `pnpm check` 用于发布前、明确要求全量验证或无法隔离影响的全栈基础设施变更；跨模块修改本身不触发全量。构建和常规检查不需要切换代理节点或发送真实模型请求。Windows 原来的原生安装缓存仍有效且桌面未升级时，无需重复 `prepare:desktop`；只有原生安装路径/版本变化或提示缺少安装准备时，才在 Windows 本机重新准备。不要复制 Mac 的模型目录缓存。

HTTP 请求省略 `stream`、传 `null` 或显式 `false` 时返回完整 JSON。OpenAI SDK 可直接用 `responses.create(...)` 读取完整响应，用 `responses.stream(...)` / `stream=True` 读取流式响应；此前依赖省略参数默认流式的客户端需补上 `stream=True`。

## 准备 Windows 开发环境

下面以 Windows x64、原生 PowerShell 和 MSVC 为例。桌面安装探测使用 Windows Appx/进程等接口，优先在原生 Windows 中开发和验收。

1. 安装 [Visual Studio C++ Build Tools](https://rust-lang.github.io/rustup/devel/installation/windows-msvc.html)，选择“使用 C++ 的桌面开发”，包含 x64/x86 MSVC 编译工具和 Windows SDK。
2. 按 [Rust 官方安装说明](https://rust-lang.org/learn/get-started/)安装 rustup，选择默认 MSVC 工具链。使用当前 stable，不要只按 Cargo.toml 中声明的最低版本安装旧工具链；本次 macOS 构建使用 Rust 1.98.1。需要 rustfmt 和 Clippy。
3. 安装 [Node.js 24](https://nodejs.org/en/download)，保持项目要求的主版本 24；然后运行 `npm.cmd install --global pnpm@10.30.3`，与 package.json 固定版本一致。
4. 需要真实模型功能时，在这台 Windows 电脑安装、打开并登录官方 Codex 桌面客户端。源码包不携带官方程序或 macOS 登录状态。
5. 仅运行 Python 辅助脚本或制作 Windows 便携包时才需要 Python；现有 Windows 打包脚本使用 `hashlib.file_digest`，请安装 Python 3.11 或以上。继续用 ZIP 交接和保留 Git 版本记录时，也可安装 Git。

安装后重新打开 PowerShell；编译时若找不到 MSVC/link.exe，使用 Visual Studio 的“Developer PowerShell”。环境可按 [Microsoft 的 Rust/Windows 说明](https://learn.microsoft.com/en-us/windows/dev-environment/rust/setup)检查。pnpm 的安装方式参考 [pnpm 10 文档](https://pnpm.io/10.x/installation)。

## 解压、构建和运行

将 ZIP 解压到用户可写目录，例如 `C:\dev`，进入解压后的 `codex-relay`。不要把旧 Mac 的 node_modules、target、data 或 .cache 复制进去。先确认所在目录同时有 Cargo.toml、package.json 和 pnpm-lock.yaml。

在 PowerShell 中逐行运行；每条命令成功后再继续：

```powershell
cd C:\dev\codex-relay
node --version
pnpm.cmd --version
rustup show active-toolchain
rustup component add rustfmt clippy
pnpm.cmd install --frozen-lockfile
pnpm.cmd run build:release
```

期望 Node 为 v24、pnpm 为 10.30.3，Rust 工具链以 `pc-windows-msvc` 结尾。`.cmd` 后缀用于直接调用 npm/pnpm 的 Windows 命令包装器，避免 PowerShell 将同名 `.ps1` 包装器视为需要执行策略许可的脚本。

构建成功后，可在终端前台运行：

```powershell
pnpm.cmd start
```

打开 `http://127.0.0.1:4317/`。默认本机免登录；如需后台运行和自动打开页面，可在构建后双击根目录的 `启动中转站.cmd`。关闭浏览器不会停止后台服务；通过管理页面的“停止中转站”正常退出。

要接入模型，保持官方桌面运行，在另一个 PowerShell 中进入同一项目目录并运行：

```powershell
pnpm.cmd run prepare:desktop
```

也可在管理页面检测原生安装。随后添加本机配置实例，或添加并登录/绑定 OAuth 账户，启用接入，为对应分组创建 API Key。Windows 的安装路径、签名摘要和认证情况必须在本机重新取得，不能复用 macOS 的安装缓存。准备命令只检测原生安装，不等于已配置可用推理路由。

客户端身份检测先读取 `package.json.main` 定位启动入口目录，再扫描该目录及子目录中的 JS/CJS/MJS 脚本，按内容寻找唯一的初始化声明；不依赖目录名、文件名前缀、哈希后缀或分块编号。支持 26.1016 的内联 `clientInfo`、26.1021 起的 `--startup-client-info` + `JSON.stringify` 参数及 26.1027 的新分块布局。初始化声明的结构保持不变时，入口目录或文件改名、拆分后可重新检测，无需为文件名变化重新编译。扫描排除 `node_modules`，最多 256 个脚本、总计 128 MiB，仍核对原生客户端名称/标题和有效的 Electron 包版本，不执行归档中的 JavaScript。如果协议或身份声明结构本身变化、内容有歧义，则明确报错，需要更新适配；无需删除账户数据或重新登录。

只读验证当前安装包，可将 `BRIDGE_NATIVE_ARCHIVE` 设置为实际 `app.asar` 的绝对路径，再运行 `node scripts/cargo-with-memory.mjs test --locked --lib installation::tests::installed_archive_identity_is_recognized -- --ignored --exact --nocapture`。此检查不修改安装配置，也不启动桌面或发起模型请求。

## 继续开发和验证

Rust 后端入口在 `crates/relay/src/`，React 在 `web/src/`；`legacy/` 是保留的旧 TypeScript 实现，默认服务不从那里启动。跨平台逻辑和 Windows 条件分支应一起维护；macOS 专属实现位于 `platform/macos.rs` 和 `process_tree/macos.rs`。

后端修改后重启 `pnpm.cmd start`。前端开发时可同时在第二个终端运行 `pnpm.cmd dev:web`，使用终端显示的 Vite 地址；API 代理指向本机 4317。完成后执行 `pnpm.cmd build:web` 更新正常管理界面。

日常按改动选择 `pnpm.cmd check:quick:rust` 或 `pnpm.cmd check:quick:web`，前后端都改才使用组合入口 `pnpm.cmd check:quick`，再运行相关回归。以下是两个独立示例，按本次修改选择；管理接口字段变化还须运行 `pnpm.cmd test:contracts`，纯文档只检查内容与链接：

```powershell
# 后端队列修改
pnpm.cmd check:quick:rust
node scripts/cargo-with-memory.mjs test --locked --test queue --test queue_control
# 前端登录页修改
pnpm.cmd check:quick:web
pnpm.cmd test:web account-login.spec.ts
```

需要官方安装和本地合成上游的测试在常规检查中保持 ignored。Windows 上重新准备原生安装后，可另行运行以下合成验证（不发送真实模型请求）：

```powershell
$env:BRIDGE_NATIVE_INSTALLATION = (Resolve-Path '.cache/launch-config.json').Path
node scripts/cargo-with-memory.mjs test --locked -j1 --test native_direct --test native_responses --test oauth_accounts -- --ignored --test-threads=1
```

macOS 验收不能代替 Windows 验收；原生安装、OAuth、进程回收和桌面调试应按实际改动在 Windows 本机验证。

## 源码与便携包

源码 ZIP 中的 `SOURCE-MANIFEST.json` 记录包内文件的大小、SHA-256 和 Unix 执行权限，用于核对包内容。

在任一平台打包源码，可执行 `python scripts/package_source.py`（Python 3.9+）；脚本支持没有 .git 的交接目录，并排除运行数据和平台构建产物。输出在 dist 中。

Windows 上生成给最终用户的便携程序包，需要先确认 Python 3.11+ 和依赖可用，再运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/Package-Portable.ps1
```

这是 Windows x64 便携包流程。macOS 的 `.command` 启动器和打包器仍保留在源码里，供回到 Mac 开发时使用；它们不在 Windows 上执行。
