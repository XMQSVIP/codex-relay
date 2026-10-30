# 原生会话能力与缓存边界

本表对应当前桥接实现及安装版 `codex-cli 0.165.0-alpha.1.2` 的隔离模拟验证。官方 RPC 可以管理会话，但不能保证上游 Token 缓存命中；同一线程、同一账户和公共前缀分别是不同条件。编辑历史后仍可能命中未变化的前缀，重建线程也不代表缓存必定为零。实际缓存用量以服务方报告为准，未知值保持未知。

## 能力对照

| 场景 | 官方能力 | 本项目行为 |
| --- | --- | --- |
| 完整历史追加用户消息 | 同一 thread 发起下一 turn | 校验身份、账户路由和完整历史后复用，只注入新增内容 |
| 同一 Key 改分组 | thread 本身不绑定中转站分组 | 新分组仍允许原账户和模型时复用存活线程及工具回调；WebSocket 保留连接历史，新请求按新分组准入和记账 |
| 原生自动压缩后继续 | 原生在内存中替换上下文，thread 可继续使用 | 保留确认完成的压缩检查点；客户端仍回传完整历史，旧历史不重新注入 |
| 工具调用、结果、继续生成 | 回答挂起的原生回调，继续同一 turn | 释放工具等待期的请求额度，下一请求准入后领取；支持多工具、连续工具轮次、custom/namespace 和等价图片结果 |
| 完成轮次后修改推理强度、摘要、服务等级 | `turn/start` 的 effort/summary 与逐轮 serviceTierForTurn | 继续同一线程；省略参数时恢复已确认的配置默认值，默认未知时重建 |
| 完成轮次后修改 JSON Schema | `turn/start.outputSchema` 仅作用于该 turn | 文本和 JSON Schema 之间可切换；`json_object` 依赖线程指令，切换时重建 |
| 追加 system/developer 消息 | `thread/inject_items` 可追加历史条目 | 完整旧历史未改动且同时追加用户消息时复用；修改已有指令内容仍重建 |
| 交回工具结果时修改推理强度、摘要、服务等级 | `turn/settings/update`，需 `step_model_switching` | 在线程配置中启用官方功能；所有结果和身份校验通过、取得新请求额度后更新，确认 `applied` 才回答回调 |
| 工具等待期修改输出格式或工具目录 | live settings 不支持这些字段 | 完整历史重建；不修改运行中的工具注册或输出约束 |
| 完成轮次后修改模型 | `turn/start.model` 可切换下一轮模型 | 显式提供推理强度，且目标模型在进程已净化的目录内时可继续同一线程；仍校验账户、Key、当前分组授权和完整历史 |
| 工具等待期修改模型，或切换模型时省略推理强度 | 运行中模型切换与新模型默认值需要额外约束 | 按完整历史重建，不继承旧模型的默认推理强度 |
| 修改 verbosity、顶层 instructions | 配置属于线程 | 按线程配置隔离，完整历史重建 |
| 客户端编辑、删减或替换旧历史 | `thread/inject_items` 只能追加；已加载线程不能通过 resume 替换历史 | 重建独立线程，保证模型看到客户端实际提交的历史 |
| 回退到旧轮次 | `thread/revert` 要求已保存的 paginated 历史；ephemeral 不支持 | 保持普通对话正文不落盘，回退后按客户端完整历史重建 |
| 从旧轮次分支 | `thread/fork` 读取源线程存储历史；隐藏的实验内存存储也可工作，并非一定需要磁盘 | 当前普通 ephemeral 线程不提供源存储历史；子/分支身份独立，首次可继承父账户，聊天状态按完整历史重建 |
| `thread/resume.history` | 官方标为不稳定、Cloud 专用；不能替换已加载线程；源码将外部历史按 Forked 初始化 | 不作为保持原线程 ID 的历史替换入口 |

工具等待期的参数更新只影响之后的生成，不改变已发送响应。RPC 拒绝、目标不可用、返回未知状态、连接错误或取消都会结束当前请求并清理所属线程；不回答剩余回调，不自动重放模型请求。输出格式、工具定义、逻辑身份、Key、当前分组授权、账户代次和完整历史校验继续生效，兄弟线程不能领取回调。

账户软亲和默认 24 小时滑动有效期，可按整数小时配置；原生工具等待上限为 5 分钟；原生进程按资源、健康和配置变化轮换，没有固定 30 分钟寿命。这些均不控制上游 Token 缓存有效期。客户端 `prompt_cache_key` 只影响本地账户选路，不用于授权领取线程，也不声称控制官方上游缓存键。

本文的正文不落盘规则描述原生执行缓存和历史存储。管理员显式开启[请求原文调试](DEVELOPMENT-CHECKS.md#请求原文调试)时，会将客户端请求及响应限长写入请求记录，独立于线程缓存；关闭或重启后清空，调试记录不用于恢复线程或工具回调。

原生账户的显式压缩复用 `profiles/standalone.rs` 的账户认证、代理、槽位与取消清理，通过 HTTP 向官方 Responses 上游提交唯一的 `compaction_trigger`。正文和返回条目仅保存在请求内存，不创建压缩线程或 rollout；加密条目及其字段原样返回，不合成密文。上游失败或断线不重放请求，终态用量单独结算，未知缓存用量保持未知。 普通 Responses 线程使用 ephemeral，内部自动压缩可继续同一线程。导出客户端配置启用远程压缩 v2；客户端安装压缩结果并替换历史时，中转站按完整历史重建原生线程。

## 按资源压力回收

原生缓存优化开启时，每个共享 Responses 进程默认容量为 64 个存活线程及 128 MiB 上下文数据，可通过 `BRIDGE_NATIVE_MAX_THREADS`（1–256）和 `BRIDGE_NATIVE_CONTEXT_BUDGET_MIB`（32–1024 MiB）调整，重启生效。资源充足时没有空闲线程固定 TTL；资源不足时优先淘汰同一逻辑身份已被后续成功对话取代的空闲线程，再按最近成功使用顺序淘汰。这里的先后版本指同一对话的历史状态，不涉及旧软件或数据库格式兼容。编辑前的线程在资源充足时仍保留，但不会被当作修改后历史直接续用。正在生成及等待工具回调的线程不属于普通淘汰候选。

普通输入只计新增原生条目，工具续接只计实际回调结果，原生输出去重记账；回传完整历史不重复计入预算，Token 用量结算保持原协议。单线程超预算在完成后关闭；工具等待超预算通过原有中断守卫清理。64/128 为工程起始值，不是最优值或 RSS 硬限制。

官方卸载通过取消订阅并确认关闭完成，不重建兄弟线程。确认后清除 RPC 事件通道、回调索引和预算；5 秒内无法确认则按已有机制退役进程。临时会话目录仍等进程退出才释放，每进程累计 256 个已卸载目录后轮换。额外进程空闲 120 秒回收，账户亲和 TTL 和上游缓存有效期各自独立。

存活线程预算包含预留、运行、空闲、等待工具和关闭中的状态；仅在确认原生线程关闭后返还容量。取消订阅 RPC 成功不是关闭确认，必须收到 `thread/closed` 或通过完整分页的 `thread/loaded/list` 确认线程已不存在。`BRIDGE_NATIVE_CACHE_REUSE=0` 时，上述两项启动配置不生效，使用 32 线程／64 MiB 累计预算与关闭优化时的工具收尾策略。

## 验证方法

### 内存存储的回退限制

安装版 `0.165.0-alpha.1.2` 的 `probe_native_memory_revert.py` 单独验证了官方隐藏内存存储（`experimental_thread_store.type=in_memory`）：

- 创建 paginated 线程失败：要求存储后端支持 `thread/turns/list` 和 `thread/items/list`。
- legacy 内存线程执行 `thread/revert` 失败：该接口仅支持 paginated 线程。
- 已加载线程通过 `thread/resume.history` 替换历史被拒绝。
- 取消订阅并确认线程卸载后，再带修改历史 resume，仍失败：`rollout path missing`。源码 `resume_thread_from_history` 将历史作为 `InitialHistory::Forked`，也不保证保留传入的线程 ID。

因此当前差距不只是桥接层尚未调用回退 RPC。保留原版 app-server 且正文完全不落盘时，所测官方内存后端尚不能完成同 ID 回退。不能通过放宽完整历史校验或伪装线程 ID 来实现。实验内存 fork 已能工作，不代表分页回退或任意历史替换同样可用。

最新探测报告位于 `.cache/native-memory-revert.json`：2 次合成模型请求、0 次真实模型请求；进程运行中及退出后的扫描未发现测试正文标记落盘（运行中只有原生锁文件不可读）。没有修改生产执行逻辑或启用实验存储。

所有验证连接 localhost 合成上游，不读取真实凭据、不消耗真实模型额度。模拟缓存数字只用于检查透传与结算，不证明真实命中率改善。

以下命令按实际改动选择；资源回收相关库测试不要求官方安装，后续 Python 验收和探测需要安装版 app-server。只运行脚本所需的二进制构建及对应场景，不因修改本文执行它们。

```sh
pnpm check:quick:rust
node scripts/cargo-with-memory.mjs test --locked --lib native_cache::
node scripts/cargo-with-memory.mjs test --locked --lib profiles::resources::
node scripts/cargo-with-memory.mjs test --locked --lib rpc::reclamation::
node scripts/cargo-with-memory.mjs test --locked --lib responses::
node scripts/cargo-with-memory.mjs build --locked
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --turn-settings-only
python scripts/test_native_cache_audit.py --binary target/debug/codex-relay.exe --cases turn_model
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --desktop-cache-only --desktop-case turn_model --desktop-first-reasoning
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --mixed-models-only
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --group-continuation-only
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --thread-reclamation-only
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --compaction-only
python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --compaction-only --compaction-position after
python scripts/probe_native_history_capabilities.py
python scripts/probe_native_memory_revert.py
python scripts/probe_native_live_settings.py
```

历史能力探测仅用合成文本比较 ephemeral 和持久源线程，测试临时目录退出后清理；生产普通对话不因此启用历史保存。参数验收覆盖 JSON、SSE、Responses WebSocket、默认值恢复、原始前缀、原生线程及缓存身份和模型请求次数。默认 Rust 测试不能替代安装版验证。

## 官方依据

- [App-server 会话、轮次、分支与设置接口](https://learn.chatgpt.com/docs/app-server)
- [官方 turn 协议定义](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/turn.rs)
- [官方线程请求处理](https://github.com/openai/codex/blob/main/codex-rs/app-server/src/request_processors/thread_processor.rs)
- [官方 live settings 回归](https://github.com/openai/codex/blob/main/codex-rs/app-server/tests/suite/v2/turn_settings_update.rs)
- [Prompt caching 的公共前缀条件](https://developers.openai.com/api/docs/guides/prompt-caching)

官方能力和安装版行为可能变化；扩展支持时应同时检查协议、实现及安装版抓包，不能只根据方法名或模拟 RPC 测试判断可用。远程压缩结果导出的限制见下节。

## 远程压缩结果导出

在源码 `23934d31b15b1127c83535f84854e5009f7cd51d` 及安装版 `0.165.0-alpha.1.2` 的核对范围内，`thread/compact/start` 可以完成远程压缩，但 RPC 通知不交出真实加密压缩条目。隐藏的内存存储能支持部分 legacy 读取、恢复与分支，仍不提供所需的分页条目；开启 raw 事件也不能让收集器导出原本未发送的条目。

依据见 [远程条目收集](https://github.com/openai/codex/blob/23934d31b15b1127c83535f84854e5009f7cd51d/codex-rs/core/src/compact_remote_v2.rs#L419)、[RPC 压缩入口](https://github.com/openai/codex/blob/23934d31b15b1127c83535f84854e5009f7cd51d/codex-rs/app-server/src/request_processors/thread_processor.rs#L2570) 与 [条目读取](https://github.com/openai/codex/blob/23934d31b15b1127c83535f84854e5009f7cd51d/codex-rs/app-server/src/request_processors/thread_processor.rs#L3685)。本项目以账户认证的 HTTP 显式压缩传输返回真实条目，不启用实验内存存储，也不声称 RPC 已具备结果导出能力。`probe_native_memory_compaction.py`、其 `--flags-only` 模式及 `--explicit-compact-only` 验收按改动选择，入口见 [开发检查](DEVELOPMENT-CHECKS.md#验证边界)。
