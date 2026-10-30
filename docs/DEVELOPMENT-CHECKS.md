# 开发检查

当前服务与管理台共用根目录的检查入口。环境准备见 [Windows 开发](WINDOWS-DEVELOPMENT.md) 和 [macOS 开发](MACOS-DEVELOPMENT.md)；维护边界见 [后端架构](ARCHITECTURE.md)。

## 本地入口

需要 Node.js 24、项目指定的 pnpm、Rust 工具链及 `rustfmt`、`clippy` 组件。先运行 `pnpm install --frozen-lockfile`。浏览器测试默认使用本机 Chrome；未安装时可在安装 Chrome 后重试。默认检查不需要官方 Codex、真实登录或模型请求。

| 命令 | 检查内容 |
| --- | --- |
| `pnpm check:quick:rust` | 后端日常入口：内存策略测试与 Rust 格式；不编译 Rust、不运行业务回归 |
| `pnpm check:quick:web` | 前端日常入口：浏览器并发策略测试、前端格式和源码/测试类型；不启动浏览器 |
| `pnpm check:quick` | 依次执行上述两个快速入口，适合前后端同时修改 |
| `pnpm check` | 完整检查入口，依次运行后端、真实接口契约和前端完整检查，任一步失败即退出；不作为日常收尾的默认命令 |
| `pnpm check:rust` | 内存检查入口的 Node 测试、`cargo fmt --check`、所有 target 的严格 Clippy、默认 Rust 单元/集成/文档测试 |
| `pnpm test:contracts` | 在隔离数据目录中通过真实 Rust 管理路由生成响应，按前端契约校验 |
| `pnpm check:web` | 已纳入文件的格式、前端源码和测试的 TypeScript 检查、Vite 生产构建、完整 Playwright 测试 |
| `pnpm format:web` / `pnpm check:format:web` | 格式化 / 只检查当前纳入的页面与契约文件 |
| `pnpm typecheck` | 仅检查前端源码和测试类型 |
| `pnpm test:web account-login.spec.ts` | 仅运行指定浏览器测试文件 |

所有编译型 Rust 检查均经过 `scripts/cargo-with-memory.mjs`，按当前可用内存限制构建并发；Clippy 和 Rust 测试使用 `--locked`。该入口不限制测试二进制内部的测试线程数。

## 日常检查与构建复用

日常开发以本次任务实际改动为范围，运行对应快速入口和相关回归；工作区其他任务遗留的修改不自动纳入。快速检查不代替业务回归：

- 纯文档：检查内容、链接及差异即可。
- 单个页面或样式：`pnpm check:quick:web`，加对应 `pnpm test:web <文件.spec.ts>`；公共样式、导航或全局交互影响无法隔离时升级为 `pnpm check:web`，不运行 Rust 全量。
- 后端局部或跨模块改动：`pnpm check:quick:rust`，加对应 Rust 集成测试及库测试筛选，参见 AGENTS.md 的模块表。多个集成目标合并为一次 Cargo 调用，避免编译无关测试二进制；不运行浏览器全量。
- 调度、鉴权、共享进程：按改动行为覆盖完成、取消、超时、权限变化、资源释放和竞争路径；跨模块本身不触发全量。后端影响确实无法隔离时升级为 `pnpm check:rust`。
- 管理 API 字段：两侧快速检查及相关回归之外必须运行 `pnpm test:contracts`，同时验证对应前端调用方；不因此自动运行全部浏览器用例。
- 检查脚本、命令或文档：验证受影响的脚本/入口、命令有效性及文档链接，不为这些改动重跑全部业务测试。依赖、工具链或构建配置变更按受影响的一侧升级；两侧均受影响且无法隔离时运行完整检查。
- 发布前、用户明确要求全量验证或无法隔离影响的全栈基础设施变更：运行 `pnpm check`。扩大范围前说明具体原因。CI 继续执行完整检查，不降低持续集成覆盖。

开发中先完成一组相关修改，再执行回归。同一范围检查通过后，不在交付前追加无关全量检查；只有后续修改影响该范围、出现失败或疑点时才重跑。报告须区分定向检查与完整检查。

例如，仅调整调度、统计 SQL 和 API Key 路由判断，且未改变接口字段时，可以运行：

```sh
pnpm check:quick:rust
node scripts/cargo-with-memory.mjs test --locked --test operations --test queue --test queue_control --test key_concurrency --test statistics_usage --test statistics_dates --test cache_usage --test request_details --test api_key_upstreams --test client_api
node scripts/cargo-with-memory.mjs test --locked --lib operations::
```

不需要同时重跑完整浏览器套件。性能优化另跑与目标相关的性能基准，不以功能测试代替性能测量。需要 Clippy 时按目标运行，例如 `node scripts/cargo-with-memory.mjs clippy --locked --lib --test queue -- -D warnings`；所有 target 的 lint 保留在完整后端检查及 CI 中。

默认 `dev` 和继承它的 `test` 构建只生成本项目的行号调试信息，依赖不生成完整调试信息；保留调试断言与溢出检查。这样减少 Windows PDB 和链接成本，不改变 release 配置。需要完整断点与变量调试时运行：

```sh
node scripts/cargo-with-memory.mjs test --locked --profile debugging --test queue
node scripts/cargo-with-memory.mjs run --locked --profile debugging -- serve
```

首次采用新配置会重新编译；后续保持工具链、编译参数及默认 `target/` 稳定，复用增量产物。不要为每次检查创建新构建目录或清空旧缓存。`debugging` 使用独立 profile 目录；旧缓存保留，本次优化不会自动回收它们。优化收益需分别测量首次构建、无修改重跑和修改后的构建，不将首次重编时间当作稳定耗时。

同一 `target/` 上的 Cargo 构建通过文件锁串行执行。看到 `Blocking waiting for file lock on build directory` 时，先检查是否已有构建；不要为了加速再启动重复检查，也不要终止其他任务的进程。多个开发任务协调共用构建结果，避免在不同参数间反复切换。

浏览器并发按物理/进程可用内存的较小值选择：低于 3 GiB 或检测失败为 1，3–6 GiB 为 2，大于 6 GiB 最多 4，同时不超过可用 CPU 数。`BRIDGE_TEST_WORKERS=1` 可进一步降低并发，不能突破资源上限；Playwright 自身的 `--workers` 命令行参数会覆盖配置，仅用于明确的手动诊断。启动时输出实际并发。独立端口、模拟 API、失败 trace 和截图策略保持不变。

完整 Rust 回归使用 `--no-fail-fast`，一个集成测试目标失败后仍检查其余目标，最终统一以非零状态退出，便于一次查看全部失败。测试依赖特定设置时要显式配置，不依赖可变的产品默认值。

开发过程中按修改范围选择现有回归，例如：

```sh
node scripts/cargo-with-memory.mjs test --locked --test queue --test queue_control
node scripts/cargo-with-memory.mjs test --locked --lib profiles::
pnpm test:web account-activation.spec.ts account-login.spec.ts
```

前端测试启动独立 Vite 服务，默认端口为 5174，通过模拟 `/api` 响应验证界面交互，不连接现有中转站。端口占用时会直接失败，可设置 `BRIDGE_TEST_WEB_PORT` 为其他有效端口；不会自动接管或复用已有服务。失败截图和 trace 保存在 `.cache/ui-tests/`。

修改管理 API 时，同步更新前端类型与模拟响应，并运行对应 Rust 接口测试。账户、额度、请求记录和统计相关类型统一从 `web/contracts/admin.ts` 推导，`web/src/api.ts` 保留导出入口；契约验证器只用于测试，不进入管理台的运行时请求路径。

`pnpm test:contracts` 通过 `admin_contracts` Rust 集成测试调用真实管理路由，覆盖独立/本机/API Key 账户、未知/不支持/正常/阻断/读取失败额度，以及空统计、未知/零/已知用量、模型排行与趋势缓存字段、账户亲和时间设置、账户备份导出与重复账户导入结果。临时响应仅包含隔离测试数据，检查后清理。浏览器的 `api-contract.spec.ts` 用相同契约检查 `mockConsole` 的响应，并验证字段缺失、无效状态和数字字符串会被拒绝。允许额外响应字段，已声明字段仍按必填、可空和类型要求检查。

格式约定在 `.prettierrc.json`。当前逐步纳入 `api.ts`、账户详情、账户调度、统计页、用量排行、账户备份导入/凭据导出组件与辅助方法、`account-transfer.css`、`web/contracts/`、契约与账户迁移浏览器测试和契约检查脚本；命令中的文件列表是检查范围。整理其他页面时同步扩展两个格式命令，先完成纯格式整理，再单独修改行为，避免一次改动混入大量无关差异。

## 持续集成

`.github/workflows/checks.yml` 在 PR、推送到 `main` 和手动触发时执行：

- Windows 和 macOS 分别运行 `pnpm check:rust` 和 `pnpm test:contracts`，覆盖平台分支及真实管理响应。
- Ubuntu 安装与锁定 Playwright 版本配套的 Chromium 及系统依赖，再运行 `pnpm check:web`。`CI` 环境下禁止提交 `test.only`。
- 浏览器检查失败时上传 `.cache/ui-tests/`，保留截图、trace 等证据 7 天。

CI 与本地复用相同的检查命令。便携包仍由独立的 [自动打包工作流](AUTOMATED-PACKAGES.md) 构建与验证；开发检查工作流本身不设置 GitHub 分支保护，也不阻止另一个工作流上传产物。

开发检查和便携包工作流缓存 pnpm 下载仓库及 Rust 依赖构建产物。Rust 缓存按平台、工具链、锁文件、Cargo 配置和编译环境隔离，检查与 release 打包使用不同 key；Windows 静态 CRT 参数在缓存恢复前已设置。仅 main 保存缓存，PR 可读取缓存。默认不缓存项目自身的可执行文件和运行数据；首次运行或工具链/依赖变化仍需构建。缓存命中不会跳过测试。

## 验证边界

默认 Rust 检查不会运行标为 ignored 的原生安装、登录和容量测试。改动涉及原生协议、进程生命周期或容量时，按 [Windows 开发](WINDOWS-DEVELOPMENT.md)、[macOS 验证](MACOS-DEVELOPMENT.md#验证命令) 的环境要求补充相应测试，并在交付中列明实际执行结果。

原生账户亲和、逻辑身份隔离和逐轮参数变更可按范围运行以下回归；先通过内存入口构建待验收二进制，再运行需要安装版 app-server 的脚本。脚本使用隔离账户与本地合成上游，不调用真实模型，但不属于 `pnpm check` 的默认执行范围：

```sh
node scripts/cargo-with-memory.mjs test --locked --lib operations::scheduler::
node scripts/cargo-with-memory.mjs test --locked --lib responses::
node scripts/cargo-with-memory.mjs test --locked --lib native_cache::
node scripts/cargo-with-memory.mjs test --locked --lib profiles::resources::
node scripts/cargo-with-memory.mjs test --locked --lib rpc::reclamation::
pnpm test:web native-affinity.spec.ts statistics-usage.spec.ts usage-diagnostics.spec.ts
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --affinity-only
python scripts/test_native_cache_audit.py --binary target/debug/codex-relay.exe
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --compaction-only
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --compaction-only --compaction-position after
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --turn-settings-only
python scripts/probe_native_history_capabilities.py
python scripts/probe_native_live_settings.py
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --explicit-compact-only
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --thread-reclamation-only
python scripts/probe_native_memory_compaction.py
```

macOS 将 `--binary` 改为实际二进制路径，例如 `target/debug/codex-relay`。矩阵脚本可通过 `--cases` 筛选文本、工具、推理内容、参数恢复和逻辑身份用例；`--thread-reclamation-only` 调用 `scripts/test_native_thread_reclamation.py` 辅助模块，验证线程及增量字节预算、热点保留和目录轮换，辅助模块不单独运行。线程关闭确认与分页异常由 `rpc::reclamation::` 库测试覆盖。场景边界详见 [Responses 原生验收](RESPONSES.md#原生传输与兼容范围)。列出脚本或浏览器测试清单不代表已执行验收。

`legacy/` 的历史 TypeScript 回归使用 `pnpm legacy:check`，不属于当前服务的默认检查。只有修改历史实现时才需要运行。


## 请求原文调试

在管理台「系统设置 → 环境与诊断 → 请求原文调试」开启后，新请求的正文及响应内容会随请求记录保存，在「请求详情 → 请求与响应内容」查看或复制。HTTP 保留接收的正文，SSE 保留事件流；Responses WebSocket 和原生 WebSocket 以逐行 JSON 保存消息（不保留原 JSON 的空白排版）。不采集 HTTP 认证请求头。记录的是客户端与中转之间的内容，不是原生进程内部的上游抓包。

每个请求的请求侧、响应侧分别最多保存 32 MiB；截断及未收完整会在详情标注，二进制内容以 Base64 展示。采集不会等响应完整后再交付流式内容，原文在响应结束或中断后落库。开启前已开始的请求不补录，已完成且未保存原文的请求无法恢复。

关闭开关立即清空数据库中的请求与响应内容，保留统计记录；在途的旧采集任务失效，即使重新开启也不会写回。重启服务默认关闭调试并清除之前保存的调试原文。此清空是数据库逻辑删除，不代表对磁盘、SQLite WAL 或外部备份进行安全擦除。原文可能包含对话、工具参数及附件，排查完成后关闭即可。

接口为 `GET /api/settings/debug` 和 `POST /api/settings/debug`，写入参数为 `{ "enabled": true, "revision": 0 }`（revision 使用读取返回值），复用管理接口认证。不开启调试时沿用原有的失败诊断摘要。

## 安装识别与诊断界面

安装解析改动运行对应后端快速检查及 `node scripts/cargo-with-memory.mjs test --locked --lib installation::`；覆盖入口目录和脚本改名、分块布局、重复声明及扫描边界。真实安装包的只读识别是单独的 ignored 用例，设置 `BRIDGE_NATIVE_ARCHIVE` 后按 [Windows 安装识别说明](WINDOWS-DEVELOPMENT.md#解压构建和运行) 执行，不会改写安装配置或启动桌面。

诊断界面改动选择前端快速检查和 `pnpm test:web diagnostics-layout.spec.ts installation-settings.spec.ts request-debug.spec.ts`，核对宽窄屏、深浅主题、保存结果、失败及运行状态、键盘操作与焦点。模拟 API 不替代真实安装包识别；纯文档不执行这些业务检查。

## 协议探测与实验

下列入口用于修改对应协议或排查特定行为时选择执行。普通功能检查不要求逐项运行；除内存模拟实验外，原生探测需要目标机器的官方安装，使用隔离目录和合成上游，不代表真实服务端接受状态或付费能力已验证。

| 范围 | 入口 | 边界 |
| --- | --- | --- |
| 内存 RPC 模拟 | `node scripts/cargo-with-memory.mjs run --locked --example desktop_protocol_lab -- --report .cache/desktop-protocol-lab.json`；自身测试使用 `test --locked --example desktop_protocol_lab` | 初始化、时间与工具回调、用量、完成及合成证明；不读取个人凭据或调用模型 |
| 原生协议与证明 | `python scripts/verify_native_protocol.py`、`python scripts/probe_native_attestation.py` | 证明能力开关及私有 RPC 回调，不生成真实设备证明 |
| HTTP/WS 抓包 | `node scripts/cargo-with-memory.mjs test --locked --lib wire_capture::captures_real_http_ws_and_native_initialization -- --ignored --nocapture --test-threads=1` | 设置 `BRIDGE_NATIVE_INSTALLATION`；本地合成 TCP 流量、初始化、请求头及工具往返，不等同于 TLS 或官方桌面 GUI 验收 |
| 原始工具执行 | `python scripts/probe_native_original_tools.py` | 仅注入声明与动态回调的执行权差异；见 [工具协议](TOOL-PROTOCOL.md#原始工具声明与执行权) |
| 内置图片与搜索 | `python scripts/test_native_standalone_tools.py` | 隔离验证原生工具、图片保存、引用续查与取消；实际客户端用法见 [本机工具实验](IMAGES.md#本机工具实验) |

原生 API 准入与释放选择 `native_api`、`responses_api` 等相关集成目标；安装版回调和多轮选择 `native_direct`、`native_responses` 的 ignored 用例。SDK 场景和所需 Python 依赖见 [接口验证](RESPONSES-COMPATIBILITY.md#容量诊断和验证)。
