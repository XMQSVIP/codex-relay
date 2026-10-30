# Codex 桌面与直接 API 接入

客户端共用调用方密钥和 `http(s)://主机:端口/v1`，执行路径由所选账户决定：

```text
桌面界面 → 桌面官方 app-server → 中转站 → 服务端官方 app-server → 模型
SDK/curl → 中转站 → 服务端官方 app-server → 模型
桌面或 SDK/curl → 中转站 → API Key 账户配置的上游 Responses 服务
```

SDK/curl 所在机器无需安装 Codex，不需要桌面会话、专有请求头或特定 User-Agent。原生账户路径使用 prepare 检测到的官方原版程序，模型 HTTP/TLS、WebSocket、代理、认证、压缩及重试由它持有。API Key 账户已有独立的直接转发路径，无需 app-server；下文的原生 RPC 映射与限制针对原生账户，API Key 路径差异见[context_management 与直接转发](RESPONSES.md#context_management-与直接转发)。

## 桌面配置

将 [配置片段](../examples/codex-desktop-relay.toml) 合并到**客户端**的 Codex 配置，设置 `CODEX_BRIDGE_API_KEY` 为控制台创建的调用方 Key，再重新启动桌面以读取环境变量。可关闭 `supports_websockets` 改用 HTTP/SSE；两者使用同一模型、Base URL 和 Key。

服务端账户的原生配置必须指向真实模型提供方。如果客户端与服务端在同一台机器，应使用服务端独立账户配置，避免修改桌面配置时同时改变服务端模型地址。接入检测及请求调度会检查自身监听地址、配置的公开地址和本地 DNS 别名，指回本中转站时返回 `RELAY_LOOP`。无法识别外部反向代理的任意多跳回环，部署时仍需检查代理拓扑。

桌面通常会提交 namespace 工具，默认直接可用。图片、原生搜索、生图也默认自动接入，不要求验证记录、目录新鲜度或绑定默认模型；实际支持情况由原生程序和模型决定。账户“高级能力”允许显式关闭图片、搜索、生图，此类策略导致拒绝时会列出具体能力。管理端仅用 disabledFeatures 保存明确关闭项，不接受旧 features 允许列表。动态 MCP/插件函数通过客户端工具目录传入并由客户端执行，不会启用服务器上的同名 MCP 插件。

## 开发阶段约定

项目尚未正式发布，接口、配置和数据库格式按当前实现直接调整，不要求保留旧开发版本的兼容层或迁移路径；修改时同步更新调用方、测试和文档。数据库启动时初始化空库或校验当前结构，校验失败会报错并保留原数据；这项检查用于防止误读写数据。需要重新初始化开发数据时，先停止服务并备份，再由管理员处理，程序不自动清空数据。

与官方 Codex 和 Responses API 的协议适配属于当前功能，包括动态工具映射、原生 `historyMode=legacy` 压缩、桌面和直接 Responses API 两种入口。这里的 `legacy` 是原生协议参数，不表示支持旧开发版本的中转站数据库。

## 普通 API 示例

[Python 工具循环](../examples/responses_client.py)、[JavaScript 工具循环](../examples/responses_client.mjs) 均在调用方执行业务工具。最小 curl 示例（Windows 用 `curl.exe`，参数引用规则按所用 shell 调整）：

```sh
curl "$BRIDGE_BASE_URL/responses" \
  -H "Authorization: Bearer $BRIDGE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-6.1-sol","input":"你好","store":false}'
```

省略 stream 返回完整 JSON；加 `"stream":true` 返回 SSE。`store=true` 的保存要求被忽略，响应始终返回 `store:false`。HTTP 查询、删除和 previous_response_id 续接返回 404，HTTP 多轮需回传完整历史；顶层 instructions/tools 不跨普通请求自动继承。保存在 input 中的工具目录属于历史，会随历史恢复。

## 原生 RPC 映射

| Responses 语义 | 中转站调用的原生协议 |
| --- | --- |
| 初始化 | 已检测版本的 `clientInfo`，`initialize` 确认后发 `initialized` |
| 指令、工具、线程 | `thread/start`，顶层 instructions → baseInstructions；developerInstructions 清空；dynamicTools 由合并目录注册 |
| 历史及扩展输入 | 按顺序 `thread/inject_items`；按 RPC 字节预算分批 |
| 模型生成 | `turn/start`；reasoning、service tier、JSON Schema 映射到原生轮次参数 |
| 已完成轮次的参数变化 | 同一 thread 的下一 turn 可调整强度、摘要、服务等级和 JSON Schema；省略覆盖值时恢复已确认默认值 |
| 已完成轮次的模型切换 | 完整历史和所有权匹配、显式提供 `reasoning.effort`，且新模型在进程已净化目录内时，通过 `turn/start.model` 继续同一 thread；否则重建，缓存用量仍以上游报告为准 |
| 工具待答时的参数变化 | 开启 `step_model_switching`，校验整批结果后 `turn/settings/update`，确认 applied 才回复回调；只允许强度、摘要和服务等级 |
| 在途 user 文本 | `turn/steer`，携带 expectedTurnId；仅记录原生已确认的输入 |
| 客户端函数/custom/搜索 | 在真实上游响应边界向调用方返回全部工具调用；匹配等待状态时回复原生回调，其他情况按完整历史执行，不在中转站执行业务工具 |
| 断开、取消、撤权 | 共享取消 token、turn/interrupt 与拥有的进程树清理；不伪造成功 |
| 显式压缩 | 复用原生认证，以 HTTP 发送远程压缩；真实条目只经过内存 |
| 原生自动压缩后的续聊 | 将确认完成的原生压缩检查点与客户端历史指纹配对，后续只追加新输入或回复已验证的工具回调，保留同一线程；客户端改写旧历史仍重建 |

`additional_tools` 保留原始顺序，同名后续定义更新目录；namespace 增量按成员合并。`configuration_update` 保留在历史，最新 reasoning.effort 用于新轮次。`agent_message` 支持原生 input_text/encrypted_content。reasoning 支持官方 `content` 数组（`reasoning_text`/`text`）以及 summary、encrypted_content，并在原生输出转换后保留这些历史字段；compaction 及可识别的加密元数据按不透明值传递。未知语义 input 类型明确报错。

原生动态 RPC 只有 function/namespace。function/custom 的 `defer_loading` 映射到当前原生支持的 `deferLoading`；裸延迟工具通过内部命名空间适配，返回时恢复原始身份。custom 的注册采用字符串参数函数适配；返回时同时接受原生 `custom_tool_call` 和包装为 function_call 的结果，原样保留自由文本、call_id 和命名空间，不把代码当作 JSON 解析或在中转站执行。桌面通过 custom 代码工具调用 image_gen 时，代码交回桌面执行，下一轮接收真实工具结果和图片。客户端 `tool_search` 采用内部搜索函数适配，回传的 `tool_search_output.tools` 注册到后续工具目录。搜索调用 ID、执行位置、结果与工具顺序保留。原生保留 namespace 和 `mcp__` 名称使用稳定内部别名，返回时还原。`execution=server` 的历史搜索记录可回放，但注册新的服务端 tool_search 暂不支持。

第二层禁用工作区文档、宿主技能发现、shell、apps/plugins 等宿主执行能力，并把环境与工作区根设为空。原生仍可能注入权限/环境/工具上下文；内部线程 ID、原生元数据及请求头由第二层生成。自定义 grammar 作为说明传递，没有 grammar 约束解码；`json_object` 使用指令与最终 JSON 校验；`json_schema` 的严格性由原生 outputSchema 路径决定，不能独立控制 strict=false。这些都是明确的适配差异，不能等同于字节级透明代理。

`access_programs.cyber` 映射到 turn/start.cyberAccessProgram。官方原生只为自身 OpenAI 提供方发送授权程序；API Key 模式还受原生功能开关约束，自定义提供方会省略该字段。中转站保留这项原生策略。`client_metadata` 只提取 x-codex-turn-metadata JSON 字符串中的非保留字符串字段，不覆盖原生会话和轮次身份。请求详情记录参数处理结果（路径/方式/原因），不保存参数值。

请求详情中的“参数兼容说明”不等同于错误清单：`include` 不控制额外返回字段，`parallel_tool_calls` 不控制原生并行调度，`prompt_cache_key` 不控制原生缓存键；custom 的 grammar 也不保证生成格式。忽略缓存键不代表缓存关闭，工具声明出现适配提示不代表工具已经失败。各项对续聊、缓存、追踪和工具调用的实际影响见[如何理解参数兼容说明](RESPONSES.md#如何理解参数兼容说明)。

## WebSocket

连接 `/v1/responses`，提供相同 Bearer Key。消息直接包含请求字段：

```json
{"type":"response.create","model":"gpt-6.1-sol","input":"你好","store":false,"stream_id":"coding"}
```

- `generate:false` 预热只验证并保存输入/默认参数，返回空 completed 响应，不发模型推理。后续 previous_response_id 引用预热响应时可继承缺失参数；它不是上游 GPU 预热。
- 同一连接的响应可以通过 previous_response_id 续接；HTTP 查询仍返回 404。服务最多接受 32 个 Responses WebSocket 连接，每个连接缓存最多 128 条/128 MiB、每条响应及历史合计 96 MiB、TTL 1 小时；淘汰旧记录并保护各流最新续接点。缺失返回 `previous_response_not_found`，客户端重新连接后应省略 previous_response_id 并完整回放。HTTP 查询/删除不暴露连接内缓存；客户端负责保留历史。
- 同一 Key 改分组不清空连接历史；后续 `response.create` 重新检查当前分组对账户与模型的授权，并按新分组准入和结算。原账户仍获授权时，可继续使用存活的原生线程和工具回调。`response.steer` 在入队和实际执行时复核该轮准入时的权限；换组后不能向按旧分组记账的在途轮次追加输入，新轮次须重新准入。
- `stream_id` 可省略，或为 1–256 个字母、数字、`_-.`。最多 32 个命名流和一个默认流；每个流 FIFO，不同流共用账户/Key/全局并发调度。待处理队列最多 64 项/64 MiB。
- `response.steer` 包含同一 stream_id、可选匹配的 response_id、user 文本 input；转为 turn/steer。图片及工具结果通过下一轮提交。
- `response.interrupt` 必须包含 response_id 和 `mode:"discard_partial_items"`，可选 stream_id 必须匹配。第二层 turn/interrupt 确认后发 response.interrupt.accepted；失败发 response.interrupt.failed，重复已接受的中断幂等。原生实际中断产生 response.incomplete，incomplete_details.reason 为 interrupted，只将完整条目加入续接历史；用量缺失时为 null。正常完成与中断竞争以实际终态为准。中断不关闭连接，其他流继续工作。
- 可选 event_id 在连接内最多记录 4096 个，重复值拒绝；不提供跨连接或 HTTP 重试的全局 exactly-once 保证。
- 连接最长 1 小时；每条消息和定时检查重新鉴权。关闭连接取消正在执行的请求并丢弃排队请求，恢复需完整回放。

加密历史的持久化绑定按 Key、分组、账户、提供方和模型隔离，采用 30 天滑动有效期。当前作用域已有有效绑定时固定原账户，混合不同账户或路由的有效绑定仍返回 `RESPONSE_CHAIN_MISMATCH`。查不到有效绑定时，客户端可提交完整历史，按新请求正常选路并由上游判断是否接受加密内容；成功或有效的不完整响应会为已接受的加密输入和输出建立当前作用域绑定。该路径不会恢复其他 Key 的线程、回调或连接历史，请求发出后也不自动换账户重放。持久化绑定与存活线程的区别见 [保存与生命周期](RESPONSES.md#保存容量与生命周期)。

新响应超过连接缓存预算，或没有足够可淘汰记录时，会跳过本次缓存并交付已经完成的回复；后续无法按该响应 ID 续接时，客户端应完整回放历史。连接缓存与响应绑定不提供正文查询服务；管理员显式开启[请求原文调试](DEVELOPMENT-CHECKS.md#请求原文调试)时保存的请求记录另供排查。

## 压缩与终态

原生账户的显式压缩复用 `profiles/standalone.rs` 的账户认证、代理、槽位与取消清理，通过 HTTP 向官方 Responses 上游提交唯一的 `compaction_trigger`。正文和返回条目仅保存在请求内存，不创建压缩线程或 rollout；加密条目及其字段原样返回，不合成密文。上游失败或断线不重放请求，终态用量单独结算，未知缓存用量保持未知。

`POST /v1/responses/compact` 返回 `response.compaction` 对象；普通 Responses 的 JSON/SSE 及 WebSocket `compaction_trigger` 返回 Responses 事件。加密条目沿用原有 Key、分组、模型、账户及路由代次绑定。普通生成与内部自动压缩仍由官方 app-server 执行。导出配置启用 `capabilities.remote_compaction = "v2"`；API Key 直连 compact 保持原行为。

模型失败返回 response.failed 或入口 error；原生条目标记 incomplete 时保留 response.incomplete。用量来自原生事件，不根据文本估算。当前原生 raw completion RPC 不暴露全部上游 incomplete_details，所以不能恢复其未提供的终态原因；原生错误会明确作为失败返回。

`NATIVE_TOOL_SCOPE` 表示本轮返回的工具不符合调用方工具目录或工具策略，并不表示网络连接故障。JSON、SSE 和 WebSocket 保留具体校验原因及有长度限制的工具 name/namespace，不回显参数或代码。模型沿用提示词里的原始 function/custom 名称时，适配器允许精确匹配本轮已声明的工具；原生别名映射优先，RPC 回调仍只允许实际注册的原生身份。未声明工具、错误命名空间及 `tool_choice=none` 继续拒绝。该映射和失败诊断由原版 app-server 与模拟模型测试覆盖，实际桌面操作仍需现场验证。

默认 namespace、工具结果身份、错误参数回放和重复调用 ID 的检查范围见[工具协议与排障](TOOL-PROTOCOL.md)。

## 容量、诊断和验证

正文、解压后正文、合并历史和累计输出各最多 32 MiB，历史/输出最多 16,384 项；原生帧最多 8 MiB，历史分批注入。工具目录最多 1,024 个，函数名最多 128 字符、命名空间最多 64 字符。图片与工具数量限制见 [Responses 接口](RESPONSES.md)。HTTP 支持 identity/gzip/zstd，并限制解压窗口与膨胀后的字节数。

`GET /api/capabilities` 的 automaticCapabilities 为 true，账户记录有效 enabledFeatures 和明确 disabledFeatures。模型目录只辅助选路，付费能力诊断是可选排障工具，不改变开关。`responsesAdapter` 记录已检测原生版本、SHA-256、对照源码 revision，以及 `native`（原生支持）、`adapted`（适配支持）、`unsupported`、`unverified`。升级成未核验的二进制后，支持项变为 unverified，应重新执行协议兼容测试；此诊断状态不会成为客户端调用的验证门禁。

初始协议映射的实现依据为官方源码 `adca78ddae79568279d0731dfdebef1df16f3cde` 和本机 `codex-cli 0.164.0-alpha.11.1` 的 schema、原生 RPC；后续线程复用、模型切换和设置更新的安装版依据见 [原生会话能力对照](NATIVE-CACHE-CAPABILITIES.md)。日常按实际改动选择后端快速检查及相关目标，例如：

```powershell
pnpm check:quick:rust
node scripts/cargo-with-memory.mjs test --locked --lib responses::
node scripts/cargo-with-memory.mjs test --locked --test responses_api --test responses_tools --test responses_websocket
```

需要安装版 app-server 的原生或 SDK 验收另按任务选择，使用隔离配置、模拟模型与合成凭据，不调用真实付费模型；以下 Windows SDK 场景先构建脚本默认使用的 debug 二进制：

```powershell
node scripts/cargo-with-memory.mjs build --locked
$env:BRIDGE_NATIVE_INSTALLATION=(Resolve-Path .cache/launch-config.json).Path
node scripts/cargo-with-memory.mjs test --locked --test native_responses -- --ignored --nocapture
npm install --prefix .cache/responses-sdk openai
python -m pip install openai httpx websocket-client zstandard
# 填本机已分配的私有 IPv4；此脚本只启动临时服务。
python scripts/test_responses_sdk.py --model-host <本机私有IPv4>
```

前端改动另运行 `pnpm check:quick:web` 及相关浏览器测试，管理接口字段变化必须执行 `pnpm test:contracts`；纯文档只检查内容与链接。完整检查的适用条件见 [开发检查](DEVELOPMENT-CHECKS.md)。

原生集成验证模型连接 PID 确实属于官方二进制。SDK 脚本覆盖普通 Python/JavaScript、原始压缩 HTTP、JSON/SSE、工具往返/并行、压缩恢复、WS 预热/多流/重连/追加输入/撤权，并启动第一份官方程序按桌面 RPC 发起编程流程，对比直连与经过第二份官方程序的结果。覆盖项还包括官方 turn/steer 触发 response.interrupt、重复中断与流隔离、notify 多次输出、裸延迟工具往返及多工具、多图片输入。HTTP 查询/ID 续接检查预期为 404；文本与加密历史检查完整回放。其他原生测试覆盖图片、namespace/custom、工具搜索、结构化输出。GUI 中的真实账户、真实模型与完整插件生态验收仍需单独执行，自动测试不宣称已覆盖它们。

桌面现场验收可依次进行：连接配置 → 文本对话 → 编辑临时项目并运行测试 → 图片 → MCP/插件工具 → 压缩后续聊 → 停止生成 → 重连恢复。观察控制台请求记录和客户端实际工具执行结果。Chat Completions、实时语音、托管 MCP/file_search、远程图片和文件输入不在本次支持范围。
