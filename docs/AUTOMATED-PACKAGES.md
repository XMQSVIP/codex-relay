# GitHub 自动构建便携包

工作流位于 `.github/workflows/portable.yml`，全部使用 GitHub 托管机器，无需连接个人电脑或服务器。

PR 和 `main` 的源码回归由独立的 `.github/workflows/checks.yml` 执行，覆盖 Windows/macOS/Linux Rust 检查及管理台浏览器测试，见 [开发检查](DEVELOPMENT-CHECKS.md)。两个工作流独立触发；便携包验证结果不代表源码回归已通过。

推送到 `main`、推送 `v*` 标签或在 Actions 页面点击 **Run workflow** 时，会分别构建：

| 下载名称 | 适用平台 | 构建机器 |
| --- | --- | --- |
| `CodexRelay-windows-x64-…` | Windows x64 | `windows-2022` |
| `CodexRelay-macos-arm64-…` | Apple Silicon Mac（M 系列） | `macos-15` |
| `CodexRelay-linux-x64-…` | Linux x86_64（含 Alibaba Cloud Linux 4） | `ubuntu-24.04` |
| `CodexRelay-linux-arm64-…` | Linux aarch64 | `ubuntu-24.04-arm` |

进入仓库 **Actions → Build portable packages → 对应运行 → Artifacts**，选择平台下载。每个下载包含实际便携 ZIP 和 `.sha256` 校验文件；验证结果保留在构建日志中，不在 ZIP 旁生成 JSON 报告。若下载得到外层归档，先解压外层，再完整解压里面的便携 ZIP。Actions 产物保留 30 天，过期后可手动重新构建。

更新项目版本并推送到 `main` 后，同一提交的四平台打包和 `Development checks` 都通过时，自动创建 `v<项目版本>` 标签，并复用本次已验证的 ZIP 和 `.sha256` 文件创建正式 GitHub Release，无需手动推送标签或配置额外令牌。检查未完成时最多等待 30 分钟；检查失败或等待超时会阻止发布。当前项目版本为 `0.2.3`，对应发布标签为 `v0.2.3`。

已有 Release 或版本标签属于另一提交时跳过自动发布，不覆盖已发布版本；下一次发布须先更新版本号。若同一提交已经生成标签但上传中断，重新运行该打包工作流可以继续发布。自动发布只接受当前 `main` 的同一提交，准备发布时若 `main` 已有新提交，旧构建跳过发布。仍支持手动推送精确的 `v<项目版本>` 标签触发四平台打包和发布。

构建前核对根目录和管理台 `package.json`、Rust package 与 `Cargo.lock` 中的项目版本；版本标签必须精确等于 `v<项目版本>`，不一致时直接失败。可在本地运行 `python scripts/check_versions.py --ref refs/tags/v0.2.3` 验证。发布任务下载全部平台产物后重新校验 ZIP 的 SHA-256，通过后才上传 Release。

工作流安装 Node.js 24、项目指定的 pnpm、Python 3.12 和稳定 Rust，使用锁文件安装依赖并重新构建 release。Windows 使用静态 CRT；macOS 使用 arm64 架构并检查系统动态库依赖、执行权限和临时签名。macOS 包适用于 Apple Silicon，不生成 Intel 包；最低 macOS 版本记录在包内 `manifest.json`。Linux 使用 musl 静态链接，不依赖构建机的 glibc 版本；打包时检查 ELF 架构和静态依赖，包含前台启动器与 systemd 模板。

只有完成打包和解压启动验证的产物才会上传。验证覆盖隔离目录启动、管理页面、空账户和密钥、重复启动及服务退出（Linux 还检查管理令牌、SIGTERM 和同目录重启）；不安装官方 Codex、不登录、不发送模型请求。每个架构独立运行，一个失败不会取消其他架构；同一分支的新推送会取消该架构的旧打包任务。Release 发布任务串行执行，开始上传后不因新推送而取消。

发行包不包含运行数据或登录凭据。原生账户使用者仍需安装官方 Codex 并完成自己的账户配置；API Key 直连无需 Codex。macOS 包没有 Apple Developer ID 签名或公证，首次运行可能需要按系统提示允许。使用说明见 [Windows 便携版](PORTABLE-README.md)、[macOS 便携版](MACOS-PORTABLE.md) 和 [Linux 服务器](LINUX-SERVER.md)。
