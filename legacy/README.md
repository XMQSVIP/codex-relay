# TypeScript 历史实现

保留迁移前的协议探索、原生工具管道、共享桌面与过滤层代码和回归测试。项目默认入口已切换为 Rust，不再从这里启动服务。

在仓库根目录执行 `pnpm legacy:check` 可运行历史回归；`pnpm legacy:cli --help` 查看历史 CLI。旧文档中的 `--report` 参数仅适用于这个 CLI；Rust CLI 将报告自动写入 `.cache/rust-*-probe.json`。
