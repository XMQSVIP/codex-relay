# 后端模块与维护边界

项目保持单个 Rust 服务、单个运行数据目录和 React 管理界面。模块化在进程内完成，管理 API 和客户端协议仍使用现有入口。

Codex Relay（Codex 中转站）的 Rust 源码位于 `crates/relay/`，crate 导入前缀为 `codex_relay`。命名约定见 [项目名称与图标](#项目名称与图标)。

## 原生安装与身份检测

`installation.rs` 负责自动检测桌面、校验独立 Codex 路径及保存安装配置。桌面归档从 `package.json.main` 定位启动目录，按内容寻找唯一的客户端初始化声明，排除 `node_modules`，最多扫描 256 个脚本、总计 128 MiB；不执行归档脚本。目录或分块文件改名不要求固定文件名适配，声明结构未知或有歧义时明确报错。独立模式只校验 Codex 版本与初始化协议，不要求桌面或 ASAR。安装检测及诊断入口见 [开发检查](DEVELOPMENT-CHECKS.md#安装识别与诊断界面)。

## Operations：调度与运行记录

独立图片入口位于 `images.rs`，传输选择位于 `profiles/images.rs`，结算位于 `operations/images.rs`。兼容请求优先进入 `profiles/native_tools.rs` 的原版工具会话，其余参数在原生 turn 发出前选择 `profiles/standalone.rs` 的认证加 HTTP 直连。图片复用 Key、分组、能力开关与并发调度；原生工具使用独占槽位并在图片完成后停止进程，不重放未知结果。默认记录不保存图片正文；请求原文调试开启时按客户端请求及响应的采集规则限长保存，见 [请求原文调试](DEVELOPMENT-CHECKS.md#请求原文调试)。

外部调用方继续从 `codex_relay::operations` 导入 `Operations`、`Lease`、配置和查询类型。`operations.rs` 只声明子模块、导出公共类型，并保存跨模块共享的状态结构。子模块保持私有；原有 `runtime` 公共路径保留。

桌面独立搜索入口位于 `search.rs`，传输分支位于 `profiles/search.rs`。`upstream/bindings.rs` 以独立搜索命名空间固定账户及传输模式，只保存路由摘要。原生搜索在线程内保留引用，`warm::Worker` 的 tool_session 与 Responses 会话隔离，受同一进程池容量、空闲和健康回收边界约束；同一 Key/分组/会话/参数串行执行并验证完整上下文相同或追加。旧的持久搜索绑定保持直连。引用会话失效后明确报错，不换路径。API Key 和不兼容独立参数继续直连。`client_api.rs` 的已认证 fallback 负责记录未知路径/错误方法拒绝。

| 文件 | 修改什么功能时查看 |
| --- | --- |
| `operations/storage.rs`、`schema.rs`、`schema.sql` | 数据目录独占锁、SQLite 打开、当前结构初始化与校验、重启后的中断恢复 |
| `operations/settings.rs` | 账户与分组策略、代理/并发/队列配置、revision 校验、管理状态快照 |
| `operations/activation.rs` | 账户启用状态、保留路由配置、停用后的空闲等待与维护容量验证 |
| `operations/admission.rs` | 路由选择、权限与容量判断、公平入队、请求记录创建、即时占额 |
| `operations/scheduler.rs`、`scheduler/` | 综合评分、探索、原生账户亲和绑定、准入续期与失效、父线程继承及对应测试 |
| `operations/queue.rs` | 排队查询、管理员取消、服务退出、等待唤醒、出队占额、等待任务丢弃 |
| `operations/lease.rs` | 执行完成记录、释放额度、未完成租约被丢弃时的取消记录 |
| `operations/history.rs` | 请求历史筛选与分页、汇总与趋势、空闲时清空历史 |
| `operations/request_content.rs` | 失败请求参数与响应的脱敏、裁剪及按需读取 |
| `debug_capture.rs`、`operations/debug_content.rs` | 可选请求原文采集、按调试版本写入、关闭或重启后的清理；不参与续聊恢复 |
| `operations/api_keys.rs` | API Key 创建与修改、认证与授权 |
| `operations/inference.rs` | 推理路由管理、用量字段与汇总、模型执行结果记录 |
| `operations/telemetry.rs` | 执行阶段、时延、交付与清理结果 |
| `operations/runtime.rs` | 原生模型能力目录、账户健康状态、冷却与能力校验 |
| `operations/rate_limit.rs` | ChatGPT 登录账户的短期限流退避、账户隔离及单请求恢复探测；API 账户排除 |

## 状态一致性约束

- `Operations::inner` 中的配置、活动请求、等待队列、轮询游标和数据库写连接共享同一把锁。容量检查与占用、取消与出队、账户迁组与请求准入必须在同一个临界区完成。
- 持有这把同步锁时，不执行原生进程启动、模型请求或其他异步等待。等待者先注册通知，再检查容量，避免错过完成通知。
- 调度路径先写请求记录，再发布活动或排队状态。结算和取消沿用现有的存储失败处理；退出时即使写入失败，也必须释放等待者。
- `Lease` 和等待者的 `Drop` 负责清理未完成工作。修改这些路径时同时检查正常完成、取消、超时和任务被丢弃的情况。
- 已配置路由的账户可分配多个客户端执行租约；登录、凭据导入与原生维护租约保持账户级互斥。冷却恢复阶段额外限制一个探针请求，不依赖账户并发值等于1。

- 数据库由 `storage.rs` 打开，`schema.rs` 初始化并校验当前结构，建表定义位于 `schema.sql`；增加字段时同步更新定义与校验，不在查询路径临时建表。项目尚未正式发布，结构可直接调整，无需维护旧开发数据的迁移路径；校验失败保留数据并报错，不自动清空或重建，见 [开发阶段约定](RESPONSES-COMPATIBILITY.md#开发阶段约定)。
- 历史统计与账户用量使用独立的 SQLite 只读连接，在 WAL 读事务中取得一致快照；读取不访问 `Inner`。管理接口先异步排队再进入后台阻塞线程，最多一个统计任务执行。所有调度与写入仍共用 `Inner` 锁。
- 自动历史清理在后台线程中分批执行，每批最多 1,000 条并释放调度锁。接收请求不触发清理；手动修改保留天数仍在同一事务内保存设置并立即清理。
- 首次发布前可按设计需要调整对外字段、错误码、数据库格式和公共导入路径，并同步更新调用方、测试与文档，无需为开发版本保留兼容层。模块之间的内部辅助方法最多使用 `pub(super)`，不因拆文件扩大对外 API。

模块拆分不改变调度状态的锁顺序与一致性约束。只读历史快照与调度写入分离，但容量检查、记录写入和状态转换仍在同一临界区内完成。

软账户亲和的建立和续期位于请求记录写入成功后的准入临界区，不能在响应完成时回写。绑定按 Key、模型和对话身份隔离，分组变化后仍须重新校验账户授权；父线程提示只能影响首次账户选择。亲和设置持久化，绑定仅保存在内存中；详细规则见 [多账户智能调度](OPERATIONS.md#多账户智能调度)。

### 账户内执行器池

`profiles/pool.rs` 按账户管理有界进程槽位。普通 Responses 通过 `profiles/shared.rs` 共享兼容进程：账户准备锁和槽位锁只覆盖进程选择、必要的启动、会话领取与上下文预算预留；认证和配置 RPC 在释放这些锁后并行执行。请求在释放槽位前取得活动读租约，覆盖准备与模型执行，进程退出持有写租约。续聊匹配使用锁外计算的完整会话快照，并在领取时校验快照仍一致；预算耗尽只拒绝新预留，已预留的请求可完成准备和执行。一个进程可同时运行多个独立 thread；续聊缓存按 Key、账户、参数与完整上下文匹配，分组使用当前授权重新校验。原生长会话仍独占槽位；原生显式压缩复用 standalone 原生认证，以 HTTP 直接转发上游，正文仅经过内存。进程池最多增长到配置允许的并发高水位；每个启用的原生账户预热并保留一个通过认证和路由校验的推理实例，不发送模型请求。常驻实例不因 120 秒空闲回收，额外实例仍按空闲策略回收；原生缓存优化开启时，每个共享 Responses 进程默认保留 64 个存活线程和 128 MiB 上下文数据预算，资源不足时逐线程淘汰，同一逻辑身份被后续成功对话取代的空闲线程优先，其余按最近成功使用顺序，健康进程不因固定年龄轮换，轮换等待在途请求结束。后台每 30 秒检查并在账户空闲时补齐缺失或失效的实例；停用、暂停分组、删除及服务退出不补建。路由校验与配置摘要复用同一次 config/read 的原始配置快照，每次请求仍检查认证和配置变化。停止账户先取消所有请求再等待进程清理。

`profiles/resources.rs` 维护带记录 ID 和版本的线程账本，预留、生成、工具等待、空闲和清理状态都占额度。只累计新增原生输入、工具结果及去重后的原生输出，完整历史回传不会重复记账。自动压缩不猜测可释放字节数；预算不等于 RSS。普通回收仅处理空闲线程，取消订阅 RPC 成功后仍须确认 `thread/closed` 或完整分页的 `thread/loaded/list` 中已不存在，才归还额度并释放 RPC 通道及回调索引。清理超过 5 秒则退役进程，等待兄弟请求完成；被取消的清理任务也有账户注册和活动保护。会话目录等进程树退出后释放，累计 256 个已卸载目录后安全轮换。独立图片、搜索、原生长会话和显式压缩仍按原有独占路径执行。

RPC 将 Responses 通知与工具回调按 threadId 分发到独立事件缓冲；一个线程的事件洪峰不挤占其他线程缓冲。工具回调登记和清理也按线程隔离。单请求取消通过 turn/interrupt 及对应 turn/completed 确认；不能确认时将进程标为不再复用，等待其他读租约释放后退出。每个原生线程的工作目录由进程根目录统一拥有，只有进程树退出后才释放，避免 Windows 文件占用与兄弟请求清理互相影响。

`profiles/continuations.rs` 拥有客户端执行工具期间的待答 turn。`responses/continuation.rs` 校验回调映射和整批工具结果；会话领取使用独立状态和完整历史指纹，不能用账户亲和提示直接恢复线程。HTTP 请求完成与原生 turn 完成分别处理：成功移交等待状态时，摘要保留 `waiting_for_tool` 状态和未结束的清理标志，进程活动读租约由待答状态继续持有，请求并发额度正常结算释放。下一请求完成准入及路由验证后领取回调；并发领取拒绝，已发送工具结果后不自动重试。所有权守卫覆盖任务被丢弃，清理确认之前不释放原生进程保护。保留状态也登记在账户取消注册表中，但不计入正在生成的请求数。默认等待 5 分钟，每账户 32 条、全局 1,024 条，受现有线程、字节预算及进程退役约束；退役和资源压力先取消等待状态，再等待活动生成结束。

请求注册表改为账户下的独立取消记录，拥有式RAII注册随所属任务释放；一个请求结束不会删除其他请求的取消记录。账户停止先阻止新的请求注册并取消所有已注册请求，再逐一等待所有执行器清理；删除和服务退出覆盖整个池。界面显示一个代表性空闲进程及独立的账户活动请求总数。`profiles/inspection.rs`的账户检查仍使用主槽，调度层维护互斥保证不会覆盖执行中的客户端请求。

## Responses：逻辑身份与线程复用

`responses/affinity.rs` 生成账户选路摘要，`responses/affinity/identity.rs` 从请求体、turn metadata 和请求头提取逻辑线程、会话及明确父线程身份。HTTP 和 WebSocket 逐帧请求使用相同身份规则；冲突或缺少独立身份的子线程不能领取已有聊天状态。父线程、缓存选路键和历史前缀不能替代线程或工具回调授权。

`responses/reuse.rs` 的历史指纹属于客户端已提交和已收到的完整记录；`responses/reuse/context.rs` 单独跟踪原生压缩开始、完成及检查点。确认压缩后的线程可以继续对应同一客户端记录，只注入新增内容，不将客户端旧历史重新写回压缩后的原生上下文。检查点参与会话快照相等校验，未完成或不一致的压缩不能进入复用池。工具等待期由 `Waiting::unchanged` 在后台检查及原子领取时检查异常上下文变化，清理由原有 turn 所有权守卫负责。

`responses/reuse.rs` 区分线程稳定设置与逐轮设置，完成轮次之间可由下一次 `turn/start` 应用推理强度、摘要、服务等级和 JSON Schema 变化；同时追加新 system/developer 消息不改写旧历史。工具待答期间仅允许经官方 `turn/settings/update` 确认的强度、摘要和服务等级变化，其余设置严格匹配。请求额度、所有权和全部工具结果校验必须先就绪，设置更新成功后才能回答回调。线程与回调复用继续校验 Key、账户、逻辑身份、完整历史及当前分组授权。同一 Key 改分组后，只要原账户和模型仍获授权，可续用存活线程与工具回调。工具等待期按当前 Key 权限检查，已准入请求仍使用其原授权快照；新请求按新分组准入和结算。WebSocket 连接缓存按 Key 归属，改分组不清空，撤销或删除 Key 仍清理缓存。`response.steer` 在入队及实际执行时复核该轮的准入授权快照，换组后不能向旧分组记账的在途轮次追加输入；新轮次重新准入。参数恢复和回退条件见 [Responses 接入](RESPONSES.md#原生传输与兼容范围)及 [官方能力对照](NATIVE-CACHE-CAPABILITIES.md)，相关回归位于 `responses/reuse_tests.rs`、`responses/turn_settings_tests.rs` 与 `responses/continuation/tests.rs`。

完成轮次后的模型切换通过 `turn/start.model` 实现，要求显式推理强度以及完整历史匹配。共享进程保留启动时已净化模型目录的集合；新模型不在集合中时，只在准备阶段退役并重建进程，避免原生 fallback 恢复宿主工具。切换后不保留旧模型默认强度；工具待答期间仍严格匹配模型。

## Admin：管理接口与服务生命周期

`admin.rs` 保留 `AdminState`、共享的 revision 输入类型和统一路由注册表。领域处理函数位于私有子模块，注册表通过 `pub(super)` 引用处理函数及其请求类型。`router`、`authorize`、`run_job`、`serve`、`serve_with_network`、`Job` 和 `AdminState` 的公共导入路径不变。

| 文件 | 修改什么功能时查看 |
| --- | --- |
| `admin/access.rs` | 管理令牌校验、会话状态、Host/Origin 与后台登录检查 |
| `admin/accounts.rs` | 创建与删除账户、启停、登录与凭据绑定、维护操作的结算 |
| `admin/activation.rs` | 账户主开关、等待任务完成后停止实例、停用账户的维护操作 |
| `admin/routing.rs` | 路由启用、连接测试、账户/分组检查、模型能力验证 |
| `admin/api_keys.rs` | 密钥管理接口、客户端接入地址展示 |
| `admin/operations.rs` | 调度设置、分组与账户策略、队列操作、请求历史和统计接口 |
| `admin/diagnostics.rs` | 管理概览、桌面调试、诊断任务及报告 |
| `admin/server.rs` | 监听器启动、停止请求、信号处理与服务退出顺序 |

注册表继续在同一位置给受保护的管理路由应用 `authorize` 和请求体大小限制。会话状态与登录入口、客户端 `/v1` 路由，以及最外层公网策略与安全响应头，保留各自原有边界。增加管理接口时，应把处理函数放入对应领域文件，再在受保护路由链中注册。

停止接口只发出取消信号，统一退出流程负责取消推理、释放等待队列、等待请求和任务结束，再停止桌面调试与账户进程。停止账户、取消登录和登出使用原有维护租约逻辑，不能因调度暂停或额度占满而失去清理能力。

## Profiles：账户存储、认证与原生 Worker

`profiles.rs` 保留公开的 `Profile`、`ProfileView`、`Profiles` 类型、账户状态、共享 Worker 槽位及推理注册清理守卫。模块沿用同一份记录、槽位、删除锁、推理取消表和模型目录刷新状态。

| 文件 | 修改什么功能时查看 |
| --- | --- |
| `profiles/store.rs` | 账户元数据加载、创建、来源检查、目录保护与删除 |
| `profiles/pool.rs`、`profiles/shared.rs`、`profiles/resources.rs` | 账户进程槽位、共享 Responses 准备与续聊领取、线程及字节预算、资源压力回收 |
| `native_cache.rs`、`rpc/reclamation.rs` | 缓存启动配置、取消订阅后的线程关闭确认及 RPC 资源释放 |
| `profiles/workers.rs` | Worker 槽位、启停、状态查询、推理注册/取消和整体关闭 |
| `profiles/login.rs` | 交互式原生登录、进程内令牌导入、取消登录 |
| `profiles/oauth.rs` | 持久凭据绑定、桌面登录复用、身份校验、认证后读取提供方 |
| `profiles/direct.rs` | Responses/原生会话执行、Worker 复用、受控 LAN 流式执行 |
| `profiles/compaction.rs` | 显式远程压缩 HTTP 传输、内存条目收集与用量；认证及清理由 `standalone.rs` 共用 |
| `profiles/inspection.rs` | 路由接入检查、连接测试、原生配置与模型能力验证 |
| `profiles/warm.rs` | 已有的 Worker 生存期、会话复用状态和空闲回收 |

认证结果按原生进程缓存，缓存键包含账户绑定代次及当前凭据文件内容摘要。有效期充足且绑定/凭据未变化时复用结果；过期、临近过期或摘要变化时重新读取原生认证，同一实例的并发刷新合并为一次。桌面复用及缺少过期时间的凭据最多缓存 30 秒；失败仅短暂合并，避免刷新风暴。API 密钥授权和路由策略仍逐请求校验。

- 删除账户前，Operations 必须先持久化禁用接入的意图；删除过程中保留原槽位，并按原顺序锁定 Worker 与记录，防止既有槽位引用绕过串行化。
- 停止账户先发送推理取消信号，再等待该账户的 Worker 锁；不要为新的执行路径建立第二套槽位或独立锁。
- 共享进程中的单次完成或取消只释放该请求的活动租约；进程退役、独占执行器退出及服务退出必须回收所属进程树。临时目录的释放应在原生进程结束后进行，避免 Windows 上文件占用造成清理失败。
- 推理前的准备阶段可以按现有条件重试；发出原生 turn 后不得因拆分或抽象而自动重放模型请求。
- 路由检查与模型能力验证集中到 `inspection.rs`，但凭据验证仍由 `oauth.rs` 负责。检查结果的发布、租约结算和清理错误应保留原有顺序。

## 验证

日常按 [开发检查](DEVELOPMENT-CHECKS.md) 运行对应的 `pnpm check:quick:rust` / `pnpm check:quick:web` 加相关回归；纯文档修改只核对内容、链接与差异。跨模块修改合并相关测试目标，调度、鉴权、共享进程改动覆盖涉及的边界，不自动升级为全栈测试。完整检查用于发布前、明确要求全量验证或确实无法隔离影响的全栈基础设施变更：

```powershell
pnpm check
```

该命令依次执行 `pnpm check:rust`、`pnpm test:contracts` 和 `pnpm check:web`，覆盖构建内存入口测试、Rust 格式、所有 target 的严格 Clippy、默认 Rust 测试、真实管理响应契约、已纳入文件的前端格式、源码与测试类型、生产构建和完整浏览器测试。环境要求、单项命令及 CI 说明见 [开发检查](DEVELOPMENT-CHECKS.md)。涉及特定原生安装或真实登录的 ignored 测试需要单独按 Windows/macOS 开发文档运行，默认测试通过不代表这些环境测试已执行。

重点回归测试已有覆盖：`operations` 检查原子占额、配置持久化和重启恢复；`queue`、`queue_control` 检查公平性、取消竞争和退出释放；`key_concurrency` 检查密钥额度；`clear_requests`、`request_details`、`cache_usage` 检查记录、时延和用量；`admin`、`queue_api`、`responses_api` 检查上层接口兼容性。

Admin/Profiles 拆分还需重点检查 `admin_access`、`network` 的访问边界，`account_deletion` 的删除恢复，`oauth_accounts` 的凭据绑定，以及 `access_status`、`inference`、`native_config` 的路由行为。原生进程实测仍遵循对应 ignored 测试的环境要求。

## 后续拆分顺序

已完成 Operations、Admin 和 Profiles 的内部职责拆分。后续可评估 `guard.rs` 的策略/验证职责，以及诊断和协议实验代码的独立组织方式。每一轮保持 API 兼容并运行对应回归测试，避免把协议行为修改与模块搬移放在同一个改动里。

## 项目名称与图标

展示名称统一为 Codex Relay（Codex 中转站），项目和 Rust package 为 `codex-relay`，crate 为 `codex_relay`，源码位于 `crates/relay/`，前端 workspace 为 `@codex-relay/console`。Windows 程序为 `codex-relay.exe`，macOS 程序为 `codex-relay`；包名前缀为 `CodexRelay-`，源码 ZIP 根目录为 `codex-relay/`。打包入口见 [Windows 开发](WINDOWS-DEVELOPMENT.md#源码与便携包) 和 [macOS 便携版](MACOS-PORTABLE.md)。

图标视觉源为 [原始 PNG](branding/relay-icon-master.png)，生成提示词保存在 [relay-icon-prompt.txt](branding/relay-icon-prompt.txt)。安装项目依赖与本机 Chrome 后运行 `node web/scripts/export-branding.mjs`，导出 `web/public/` 下的展示图、标签页、ICO 及 Apple 触屏图标；CI 使用配套 Chromium。预览生成于 `.cache/branding/relay-icon-preview.png`，不进入发布产物。
