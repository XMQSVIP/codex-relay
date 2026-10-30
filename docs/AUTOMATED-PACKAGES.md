# GitHub 自动构建便携包

工作流位于 `.github/workflows/portable.yml`，全部使用 GitHub 托管机器，无需连接个人 Windows 或 Mac。

PR 和 `main` 的源码回归由独立的 `.github/workflows/checks.yml` 执行，覆盖 Windows/macOS Rust 检查及管理台浏览器测试，见 [开发检查](DEVELOPMENT-CHECKS.md)。两个工作流独立触发；便携包验证结果不代表源码回归已通过。

推送到 `main`、推送 `v*` 标签或在 Actions 页面点击 **Run workflow** 时，会分别构建：

| 下载名称 | 适用平台 | 构建机器 |
| --- | --- | --- |
| `CodexRelay-windows-x64-…` | Windows x64 | `windows-2022` |
| `CodexRelay-macos-arm64-…` | Apple Silicon Mac（M 系列） | `macos-15` |

进入仓库 **Actions → Build portable packages → 对应运行 → Artifacts**，选择平台下载。每个下载包含实际便携 ZIP、`.sha256` 校验文件和 `.validation.json` 验证报告；若下载得到外层归档，先解压外层，再完整解压里面的便携 ZIP。产物保留 30 天，过期后可手动重新构建。当前工作流不创建 GitHub Release。

工作流安装 Node.js 24、项目指定的 pnpm、Python 3.12 和稳定 Rust，使用锁文件安装依赖并重新构建 release。Windows 使用静态 CRT；macOS 使用 arm64 架构并检查系统动态库依赖、执行权限和临时签名。macOS 包适用于 Apple Silicon，不生成 Intel 包；最低 macOS 版本记录在包内 `manifest.json`。

只有完成打包和解压启动验证的产物才会上传。验证覆盖隔离目录启动、管理页面、空账户和密钥、重复启动及服务退出；不安装官方 Codex、不登录、不发送模型请求。每个架构独立运行，一个失败不会取消其他架构；同一分支的新推送会取消旧构建。

发行包不包含运行数据或登录凭据。使用者仍需安装官方 Codex，并在自己的电脑完成接入配置。macOS 包没有 Apple Developer ID 签名或公证，首次运行可能需要按系统提示允许。使用说明见 [Windows 便携版](PORTABLE-README.md) 和 [macOS 便携版](MACOS-PORTABLE.md)。
