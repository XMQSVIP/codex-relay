# 构建与内存

- 执行 Rust 构建、测试、Clippy 或 `cargo run` 前，使用项目的内存检查入口：`node scripts/cargo-with-memory.mjs <cargo 参数>`。`pnpm build`、`pnpm build:release`、`pnpm test`、`pnpm check` 和启动命令已接入此入口。
- 默认按可用内存分档：低于 3 GiB 或检测失败时强制 `--jobs 1`，3–6 GiB（含边界）最多 2 并发，大于 6 GiB 不限制、保留原有构建并发设置。限制生效时尊重更低的命令行或 `CARGO_BUILD_JOBS` 并发设置，并且不超过可用 CPU 数。Windows 同时检查物理内存和虚拟内存余量。
- 单独检查可运行 `node scripts/cargo-with-memory.mjs --memory-check`。如确需直接执行 Cargo，应先检查内存，并根据分档结果限制并发。
- `cargo fmt`、`cargo metadata`、`cargo fetch` 等不编译的命令可直接执行。
- 单任务仍发生内存不足时，下一次构建可临时关闭调试符号并禁用增量编译，使用独立构建目录；不要修改用户的全局 Cargo 设置或删除已有缓存。

## 项目边界与模块导航

- 当前实现是 `crates/relay/` 的单个 Rust 服务和 `web/` 的 React 管理台。`legacy/` 仅保留历史探索与回归，除非任务涉及历史实现，不要在那里添加当前功能。
- 先阅读相关模块及测试，再修改代码。模块搬移、格式整理与业务行为修改分开进行；优先保留现有公共入口，内部辅助方法不因拆分而扩大可见性。
- 详细维护边界见 [后端架构](docs/ARCHITECTURE.md)，检查命令与环境要求见 [开发检查](docs/DEVELOPMENT-CHECKS.md)。

表中后端入口相对于 `crates/relay/src/`，前端入口相对于仓库根目录。

| 修改内容 | 主要入口 | 优先回归 |
| --- | --- | --- |
| 准入、分组、并发、排队、结算 | `operations.rs`、`operations/` | `operations`、`queue`、`queue_control`、`key_concurrency` |
| 账户启停、登录、删除、进程池 | `profiles.rs`、`profiles/`、`admin/` | `account_activation`、`account_deletion`、`oauth_accounts`、`account_concurrency` 及相关库测试 |
| Responses、工具与流式协议 | `responses.rs`、`responses/`、`rpc.rs`、`rpc/`、`streaming.rs` | `responses_api`、`responses_tools`、`responses_websocket`、`streaming` 及相关库测试 |
| API Key 直连上游与传输 | `upstream/` | `api_key_upstreams`、`client_api` |
| 管理 API、权限与网络边界 | `admin.rs`、`admin/`、`network.rs` | `admin`、`admin_access`、`network` |
| 管理台与接口类型 | `web/src/`、`web/src/api.ts` | `pnpm typecheck`、相关 `web/tests/*.spec.ts` |

表中的 Rust 名称对应 `crates/relay/tests/` 的集成测试目标。例如：`node scripts/cargo-with-memory.mjs test --locked --test queue --test queue_control`；内嵌单元测试用 `node scripts/cargo-with-memory.mjs test --locked --lib <模块筛选>`。

## 必须保持的行为约束

- 调度容量检查、占额、请求记录及队列状态转换保持在原有原子边界内。不得为了拆模块拆开 `Operations::inner` 的一致性锁；持有该同步锁时不得启动原生进程或执行异步等待。
- 修改租约、等待者和取消路径时，一起检查完成、取消、超时、任务丢弃和退出的资源释放。账户停止先阻止新注册并取消在途请求，再等待账户执行器清理。
- 共享进程的单请求结束只释放自身活动租约。进程退役等待其他活动请求结束；原生进程树退出后才释放其临时工作目录。沿用已有槽位、锁顺序和清理守卫。
- 只在现有允许的准备阶段重试；原生 turn 发出后，结果未知也不得自动重放模型请求。续聊匹配必须保留 Key、账户、参数和完整上下文隔离，并重新校验当前分组对账户及模型的授权；同一 Key 改分组后授权仍有效时，可以续用存活线程和工具回调。
- 管理接口字段变化要同步检查 Rust 序列化、`web/contracts/admin.ts`、`web/src/api.ts`、调用组件、`web/tests/fixtures.ts` 和接口回归测试。账户、额度和统计类型从契约定义推导；运行 `pnpm test:contracts` 校验真实 Rust 响应，浏览器模拟测试不能代替它。
- 项目尚未正式发布，接口、配置和数据库格式按当前实现直接调整，无需为旧开发版本保留兼容层或迁移路径；同步更新调用方、测试和文档。数据库由 `operations/schema.rs` 初始化并校验当前结构，建表定义位于 `schema.sql`；不在查询路径建表，不自动清空或重建已有运行数据。

## 验证与交付

- 日常开发默认按本次任务实际改动选择检查，不因工作区存在其他未提交修改扩大范围。仅后端运行 `pnpm check:quick:rust` 加相关 Rust 回归；仅前端运行 `pnpm check:quick:web` 加相关 `pnpm test:web <文件.spec.ts>`；前后端都改才运行 `pnpm check:quick` 加两侧相关回归。纯文档只检查内容与链接；检查脚本只验证受影响入口及脚本测试。
- 跨文件、跨模块、调度/鉴权/进程池修改本身不触发全量：按上表合并相关测试目标，覆盖具体行为及取消/超时/释放等边界。Rust 集成测试尽量合并为一条带多个 `--test` 的命令，库测试按模块筛选；不要为局部修改运行无目标限制的 `cargo test` 或 `clippy --all-targets`。需要 lint 时选择 `--lib`、`--bin` 或相关 `--test`。
- 完整 `pnpm check` 用于发布前、用户明确要求全量验证，或确实无法隔离影响的全栈基础设施变更。影响仅限后端或前端时，最多升级为 `pnpm check:rust` 或 `pnpm check:web`；先说明扩大检查范围的具体原因，不以“更保险”为由追加无关全量检查。CI 在 PR 和 main 推送时继续执行全量后端、契约和前端检查。管理接口字段变化仍必须执行 `pnpm test:contracts`。
- 同一轮修改收敛后再运行相关回归；已通过的检查只在代码再次影响该范围、出现失败或有未解决疑点时重跑。检查通过即交付，不在结尾自动补跑全量。报告实际执行范围及未覆盖的必要环境测试，不把快速或定向检查描述为全量通过。
- 默认 dev/test 构建仅保留本项目错误行号，依赖不生成完整调试信息。需要断点和变量调试时使用 `node scripts/cargo-with-memory.mjs test --locked --profile debugging`（或 build/run）；不要为普通测试频繁切换 RUSTFLAGS、target 目录或清空缓存。
- `pnpm check:rust` 覆盖内存入口测试、Rust 格式、严格 Clippy 和默认 Rust 测试；`pnpm check:web` 覆盖已纳入文件的格式、源码与测试类型、生产构建和完整 Playwright 测试。`pnpm format:web` 格式化当前纳入的页面与契约文件，范围见 [开发检查](docs/DEVELOPMENT-CHECKS.md)。
- Playwright 使用独立端口和模拟 API，不复用用户正在运行的服务。本地默认使用已安装的 Chrome；CI 使用配套 Chromium。单个前端回归可运行 `pnpm test:web account-login.spec.ts`。
- 原生安装、真实登录或容量测试中标为 ignored 的用例按任务需要单独执行；不得把默认测试通过描述为这些场景已验证。交付时说明检查结果和未执行的必要环境测试。
