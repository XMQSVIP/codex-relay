<img src="web/public/relay-icon.png" alt="Codex Relay 项目图标" width="128" height="128" />

# Codex Relay

**Codex 中转站 · 统一管理账户，为客户端提供模型 API**

Codex Relay 将本机 Codex 配置和独立 OAuth 账户接入统一的 API 入口。通过中文管理界面管理账户、分组和密钥，让客户端使用 Responses API 或 WebSocket 调用模型，查看请求状态与用量。

支持 Windows x64 和 macOS Apple Silicon（M 系列）。本机配置和 ChatGPT 账户的 Responses 请求通过官方 Codex 的 `app-server` 转发。也支持导入 API Key 账户，直接连接自定义上游，提供 Responses HTTP/SSE、WS 优先及图片接口，无需 app-server。原生账户的兼容图片、搜索请求优先由官方工具执行；蒙版、指定尺寸等不兼容参数在发送前选择原有 HTTP 直连，见 [图片接口](docs/IMAGES.md)。

[下载便携包](https://github.com/XMQSVIP/codex-relay/actions/workflows/portable.yml) · [接入文档](docs/RESPONSES.md) · [Windows 使用说明](docs/PORTABLE-README.md) · [macOS 使用说明](docs/MACOS-PORTABLE.md)

修改访问端口：编辑根目录 `relay.config.json` 的 `port` 并重启，Windows / macOS / Linux 共用。详见 [端口配置](docs/NETWORK-ACCESS.md#修改服务端口)。

## 主要功能

- **账户管理**：使用本机 Codex 配置、独立 OAuth 登录、凭据导入或 API Key 直连，查看账户状态、订阅到期日期、周期用量与剩余额度。
- **分组与密钥**：按分组调度账户，为客户端创建独立 API Key，设置模型范围和并发上限。
- **并发调度**：同一账户可同时处理多个对话，支持排队、故障冷却、额度不足时自动避让和可配置的原生账户亲和时间。
- **客户端接入**：提供 Responses HTTP/SSE、Responses WebSocket 和原生 WebSocket，支持文本多轮与客户端工具调用。
- **请求与用量**：查看请求详情，按 Key、模型和错误码筛选，统计 Token、缓存命中率及账户／密钥／模型排行。
- **网络配置**：默认允许外部访问，可配置访问权限、独立 API 端口、HTTPS 和反向代理。

图片输入、搜索和生图默认自动接入，可在账户的“高级能力”中关闭；实际可用范围取决于原生程序、模型和提供方。客户端 function/custom/namespace/MCP 工具默认可用，业务工具由客户端执行。具体支持范围见 [Responses 接入说明](docs/RESPONSES.md)。

## 快速开始

### 1. 下载并启动

在 [GitHub Actions 构建页面](https://github.com/XMQSVIP/codex-relay/actions/workflows/portable.yml) 选择一次成功完成的运行，从 **Artifacts** 下载对应平台的便携包：

| 平台 | 下载名称 | 启动文件 |
| --- | --- | --- |
| Windows x64 | `CodexRelay-windows-x64-…` | `启动中转站.cmd` |
| macOS Apple Silicon | `CodexRelay-macos-arm64-…` | `启动中转站.command` |

若下载得到外层归档，先解压外层，再将里面的便携 ZIP 完整解压到可写文件夹。双击启动文件后，浏览器会自动打开管理页面：`http://127.0.0.1:4317/`。

便携包仅包含运行必需文件和根目录使用说明；完整文档与示例保留在本源码仓库。Windows 的 `scripts/` 只保留必要启动脚本。

便携版无需安装 Rust、Node.js 或 pnpm。本机配置和 ChatGPT 账户需要官方 Codex 可执行文件，可使用独立 Codex 或桌面客户端附带的版本；仅使用 API Key 直连账户时无需安装。包内不包含官方客户端和登录凭据。macOS 包未使用 Apple Developer ID 签名或公证，首次打开可能需要按系统提示允许。

项目尚未正式发布，接口、配置和存储格式会随开发调整。开发阶段的数据需要时重新初始化，操作前先停止服务并备份。

当前下载通过 Actions 构建产物提供，工作流不创建 GitHub Release。下载和校验步骤见 [便携包下载说明](docs/AUTOMATED-PACKAGES.md)。

### 2. 添加账户

1. 自动模式下，打开并保持官方 Codex 桌面运行，完成登录或提供方配置。使用独立 Codex 时，先在“系统设置 → 环境与诊断 → 原生运行环境”选择独立模式并校验 Codex 路径。
2. 在管理页面进入“账户管理”，点击“添加账户”，选择“使用本机 Codex”。
3. 自动检测后确认账户名称和所属分组，点击“连接并启用”。
4. 需要多个账户时，可继续独立登录其他 OAuth 账户或导入自己的凭据，启用接入并分配分组。

也可在“添加账户”选择“API Key”，填写上游 API 前缀和密钥，保存后手动启用，无需检测原生安装。

本机配置支持 ChatGPT 登录、第三方 Responses 提供方和官方 API Key。多个本机配置实例读取同一份提供方配置，上游账户与额度可能共享。详细步骤见 [账户管理](docs/ACCOUNT-MANAGEMENT.md) 和 [OAuth 账户](docs/OAUTH-ACCOUNTS.md)。

### 3. 创建密钥并接入客户端

在“客户端接入”创建 API Key，选择账户所属且已启用的分组，再将以下信息填写到客户端：

| 配置项 | 填写内容 |
| --- | --- |
| Base URL | 本机默认 `http://127.0.0.1:4317/v1`；远程调用使用管理页面提供的可达地址 |
| API Key | 在中转站创建的密钥 |
| 模型 ID | 账户支持且密钥允许使用的模型 |

客户端需支持 Responses API。SDK 调用方无需安装 Codex，只需能访问中转站。

安装 Python 的 `openai` 包后，可用以下示例发起请求：

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:4317/v1",
    api_key="<中转站 API Key>",
)

response = client.responses.create(
    model="<模型 ID>",
    input="你好，请介绍一下你自己。",
    store=False,
)
print(response.output_text)
```

默认返回完整 JSON；需要流式输出时，使用 `client.responses.stream(...)` 或显式传入 `stream=True`。工具调用由客户端执行，再提交工具结果。完整示例见 [Python 工具调用](examples/responses_client.py) 和 [JavaScript 工具调用](examples/responses_client.mjs)。原生长连接客户端可使用 `/v1/native`，详见 [原生会话协议](docs/NATIVE-DIRECT.md)。

Codex 的 Responses WebSocket 使用同一 `/v1/responses` 路径，按每次请求占用并发额度；`/v1/native` 使用单独的原生会话协议，连接期间持续占用额度。可在密钥菜单中复制 Codex 配置，步骤见 [调用方接入](docs/CLIENT-ACCESS.md#使用密钥配置-codex)。

## 日常使用

- **调整并发**：在账户详情“接入与调度”中设置账户并发上限，新账户默认 5；服务总并发默认自动汇总账户容量，也可切换手动设置。同时检查 API Key 并发；请求受这三项上限共同约束，超出可用容量时排队。
- **账户亲和**：在“系统设置 → 连接与调度”设置原生账户亲和时间，默认 24 小时，可设为 1–720 个整数小时；原账户繁忙时立即分流，恢复可用后优先回到原账户。此时间控制本地选路，不保证原生线程或上游 Token 缓存保留；详见 [账户亲和](docs/OPERATIONS.md#多账户智能调度)。
- **停止服务**：关闭浏览器不会退出服务。点击管理台侧栏底部版本信息旁的电源图标，确认“停止整个中转站”；手机端先展开导航菜单。再次使用时双击启动文件，重复双击会打开现有服务页面。
- **更新程序**：停止服务并备份 `data/` 和 `.cache/`，替换程序、启动脚本和前端文件，保留自己的 `relay.config.json`。开发阶段存储结构调整后，测试数据可能需要重新初始化，程序不会自动清空已有数据。官方 Codex 桌面升级后，重新检测原生安装。
- **保存数据**：账户、密钥、配置和统计位于 `data/`，安装记录与日志位于 `.cache/`。这些目录包含本机运行信息和凭据，不要随便携包分享。
- **请求原文调试**：在“系统设置 → 环境与诊断”开启后，新请求的正文和响应会限长保存，供管理员在请求详情查看。关闭或重启服务会清空已保存原文；默认关闭，详见 [采集范围与清理规则](docs/DEVELOPMENT-CHECKS.md#请求原文调试)。
- **指定原生安装**：“系统设置 → 环境与诊断”仅提供自动模式和独立模式。自动模式识别正在运行的官方桌面，并显示检测到的 Codex、桌面程序和应用归档路径；独立模式只显示和填写 Codex 路径（Windows 为 `codex.exe`，macOS/Linux 为 `codex`，需支持 `app-server` 子命令），无需安装桌面。填写中转站所在机器上的文件绝对路径，校验通过后保存到 `.cache/launch-config.json`，重启后仍有效。独立模式沿用 `codex_desktop` / `Codex Desktop` 标识，版本读取指定 Codex 文件的实际版本，UA 仍由原生初始化返回。独立模式使用隔离目录验证版本和初始化协议，不发送模型请求；账户仍需登录或导入凭据，桌面调试不可用。新启动的原生进程使用新路径，已有进程继续运行。手动路径失效时需重新设置；选择自动模式并点击“重新检测安装”可恢复自动选择。

首次启动默认后台免登录且允许外部访问，客户端 API 始终需要 API Key；已有访问设置保持不变。在“系统设置 → 访问安全”可配置后台密码或关闭外部访问。其他电脑使用服务电脑的实际 IP 或域名连接。主端口同时提供后台和 API；需要仅开放 API 或配置 HTTPS 时，可使用独立客户端端口。详见 [网络配置](docs/NETWORK-ACCESS.md)。

## 接口与使用范围

本机配置和 ChatGPT 账户通过官方 `app-server` 建立模型连接，提供 Responses 接口的适配能力；API Key 直连账户的参数与能力由上游处理。支持的参数、内容类型和工具范围见 [接口兼容说明](docs/RESPONSES-COMPATIBILITY.md)；部分可选参数会被忽略，请以 [Responses 接入说明](docs/RESPONSES.md) 为准。

客户端业务工具在调用方执行，原生后端仍可能运行内部代码编排。进程与会话隔离不等于完整的不可信租户沙箱，部署范围应与此边界相符。详见 [工具执行边界](docs/NATIVE-DIRECT.md#工具执行边界)。

## 从源码运行

需要 Rust 工具链（Windows 使用 MSVC）、Node.js 24 和 pnpm 10.30.3。原生账户需要支持 `app-server` 的官方 Codex，可使用桌面附带版本或独立可执行文件。在项目根目录逐条执行，确认每条命令成功后再继续：

```sh
pnpm install --frozen-lockfile
pnpm build:release
# 自动模式下，先保持官方桌面运行，再检测安装
# 独立模式可在服务启动后通过管理台配置路径；API Key 直连可跳过
pnpm run prepare:desktop
pnpm start
```

`pnpm start` 使用 release 构建；调试运行可使用 `pnpm start:debug`。制作便携包可使用根目录的 `打包便携版.cmd`（Windows）或 `打包便携版.command`（macOS）。环境准备和检查命令见 [Windows 开发说明](docs/WINDOWS-DEVELOPMENT.md) 与 [macOS 开发说明](docs/MACOS-DEVELOPMENT.md)。

构建前会自动检查可用内存：低于 3 GiB 或检测失败时，Rust 构建、测试和 Clippy 使用单任务（`--jobs 1`）；3–6 GiB（含边界）最多 2 并发；大于 6 GiB 不限制，保留原并发设置。限制生效时，命令行或 `CARGO_BUILD_JOBS` 指定的更低并发会保留，且不超过可用 CPU 数；命令行设置优先于环境变量。Windows 同时检查物理与虚拟内存余量，打包和源码启动入口也使用此检查。运行 `pnpm check:memory` 可单独查看结果；环境变量 `BRIDGE_BUILD_MIN_FREE_GIB` 可调整双任务起点（默认 3），超过该值的两倍后不再限制。手动构建请使用 `node scripts/cargo-with-memory.mjs <cargo 参数>`，例如 `node scripts/cargo-with-memory.mjs test --locked`；直接执行 `cargo` 不会自动经过检查。

日常修改按范围运行 `pnpm check:quick:rust` 或 `pnpm check:quick:web`，再运行改动模块的相关回归；前后端都改可用组合入口 `pnpm check:quick`。跨模块修改合并相关测试目标，不自动触发全量。发布前、明确要求全量验证或无法隔离影响的全栈基础设施变更运行 `pnpm check`；CI 仍自动执行全量检查。默认 Rust 构建减少调试信息，完整调试可选择 `--profile debugging`；浏览器按内存与 CPU 使用 1–4 个进程。检查范围、缓存与环境要求见 [开发检查](docs/DEVELOPMENT-CHECKS.md)。

## 更多文档

| 主题 | 文档 |
| --- | --- |
| 账户、登录与额度 | [账户管理](docs/ACCOUNT-MANAGEMENT.md) · [OAuth 账户](docs/OAUTH-ACCOUNTS.md) |
| API、密钥与工具调用 | [Responses 接入](docs/RESPONSES.md) · [密钥与权限](docs/CLIENT-ACCESS.md) · [原生会话](docs/NATIVE-DIRECT.md) |
| 分组、并发与统计 | [运行管理](docs/OPERATIONS.md) |
| 网络与部署 | [网络配置](docs/NETWORK-ACCESS.md) · [Windows 便携版](docs/PORTABLE-README.md) · [macOS 便携版](docs/MACOS-PORTABLE.md) |
| 开发与维护 | [后端架构](docs/ARCHITECTURE.md) · [管理界面](docs/UI-CONSOLE.md) · [会话生命周期](docs/NATIVE-DIRECT.md#原生会话生命周期) |
