# Responses HTTP 与 WebSocket 接口

客户端可使用 OpenAI SDK，设置 Base URL 为管理页面提供的 `http(s)://主机:端口/v1`，API Key 使用调用方密钥。

Codex 桌面与普通 API 客户端共用这个 Base URL 和密钥。SDK/curl 调用方无需安装 Codex；使用原生账户时中转站服务器需要官方原版 app-server，API Key 账户则直接连接所配置的上游。[桌面配置、协议映射、压缩和验证边界](RESPONSES-COMPATIBILITY.md)；[JavaScript 示例](../examples/responses_client.mjs)。

请求有两条执行路径：**原生账户适配**（ChatGPT/OAuth 等账户，通过 app-server）和 **API Key 直连**（中转站直接请求账户配置的上游）。API Key 账户在原生参数校验前分流，不经过原生工具转换和可选参数过滤；两条路径仍执行鉴权、路由绑定、并发和大小限制。调用方密钥是中转站的接入凭据，不等于上游 API Key 账户；使用调用方密钥发请求并不能决定走哪条路径。

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:4317/v1", api_key="<调用方密钥>")
response = client.responses.create(model="<模型名>", input="你好", store=False)
print(response.output_text)

with client.responses.stream(model="<模型名>", input="写一段介绍", store=False) as stream:
    for event in stream:
        if event.type == "response.output_text.delta":
            print(event.delta, end="", flush=True)
```

## 请求与输出

| 接口 | 原生账户适配 | API Key 直连 |
| --- | --- | --- |
| `POST /v1/responses` | 省略 `stream` 或传 null 返回完整 JSON，`stream=true` 返回 SSE | 请求交给上游处理；HTTP/SSE 返回上游内容，WS 优先模式按调用方的 stream 设置转换为 SSE 或完整 JSON |
| `GET /v1/responses` | WebSocket：支持 response.create、预热、连接内 ID 续接、多流、文本 steer 和 interrupt | WebSocket：桥接 response.create 和输出事件；ID 续接依赖上游及响应绑定，预热依赖上游 WS；steer/interrupt 尚未接入上游控制 |
| `POST /v1/responses/compact` | 原生账户认证后，以 HTTP 请求远程压缩，原样返回真实压缩条目；正文不落盘 | 通过 HTTP 转发到选中账户的上游 compact 接口 |
| `POST /v1/alpha/search` | 兼容请求优先执行官方 web.run 工具会话；其余请求在发送前选择直连 | 使用上游 API Key 经 HTTP 转发；保留搜索引用与结果 |
| `GET /v1/responses/{id}`、`DELETE /v1/responses/{id}` | 不保存响应正文，返回 404 | 存在有效的直连响应绑定时转发到原账户的上游；结果由上游决定，不保证可查询或删除 |
| `GET /v1/models` | 读取账户模型提示列表 | 尝试刷新 API Key 上游目录并参与模型提示列表；上游不提供目录时仍可指定模型请求 |

### API Key 直连行为

直连路径保留 `context_management`、`include`、`parallel_tool_calls`、`prompt_cache_key`、`max_output_tokens`、`client_metadata` 和工具定义等字段，不应用下文的原生忽略或 custom 函数转换规则。参数是否被接受以及具体效果由上游决定；调用方 Key 的明确策略仍可能覆盖参数，例如开启快速模式时将 `service_tier` 设为 `priority`。保留字段语义不代表 HTTP 字节级透传：入口会解压和重新序列化 JSON，认证使用选中账户的上游密钥，浏览器 Cookie 不转发。

默认 HTTP/SSE；WS 优先模式只用于 Responses 生成，`background=true` 使用 HTTP。WS 请求会去除传输控制字段 stream/background 并封装为 response.create；下游 WebSocket 的 type/stream_id/event_id 由桥接层处理。上游明确不支持 WS 握手时可在发送生成请求前回退 HTTP，认证失败、限流或发送后断线不会自动重放。独立 compact 和响应查询/删除始终使用 HTTP。

`store` 按请求交给上游，不会强制改为 false；中转站保存响应归属和加密条目绑定，不保存完整响应正文，上游是否保存由其自身策略决定。`previous_response_id` 续接必须满足响应绑定及上游可用性；上游 WS 连接丢失后，不能只靠持久化的响应 ID 恢复，应完整回放历史并开始新链。查询/删除缺少有效绑定时返回 404，跨 Key、模型或账户配置变更不能绕过绑定校验。

完整普通文字历史中的消息 ID 不构成账户硬绑定，可以正常重新选路；原生账户仍使用已有软亲和优化。加密条目在当前 Key、分组及模型下没有有效绑定时，按新请求正常选路；上游接受后记录加密输入及输出的绑定。加密条目、工具条目和仅 ID 引用的已有有效绑定仍固定原账户，账户或路由冲突继续拒绝。完整历史回放不会恢复其他作用域的线程、连接或工具回调，请求发出后也不会自动切换账户重放。

下游 WebSocket 支持分流排队和生成事件桥接，但不是完整的上游控制帧代理。API Key 路径的 `response.steer` / `response.interrupt` 尚未转发到上游；不能套用原生账户的在途中断保证。`generate=false` 需要 WS 优先模式和可用的上游 WebSocket，否则明确拒绝，不转成普通生成请求。

原生 WebSocket 预热只准备连接内历史与默认参数，不占用模型并发额度、不排队，也不创建推理记录。混合分组有可用原生路由时优先本地预热并绑定该路由；已有响应链的路由绑定仍优先，后续生成时重新验证账户及授权。只有 API Key 路由可用时继续使用上游预热规则。

### 原生账户的参数与工具适配

以下参数映射、输入类型及工具适配限制仅适用于原生账户。

支持 `model`、文本 `input`、`instructions`、`stream`、`store`、`previous_response_id`、`metadata`、function `tools`、`tool_choice=auto/none`、`reasoning.effort/summary`、`text.format=text/json_object/json_schema`、`text.verbosity`、`service_tier`。顶层 `instructions` 映射到 `baseInstructions`（省略则设为空），清空第二层默认 developerInstructions；历史 `system` 消息转为原生能保留的 `developer` 角色。原生仍可能加入权限、环境和工具上下文，详见兼容说明。

SDK 调用 `responses.create(...)` 时可省略 `stream`，默认读取完整 JSON；需要流式响应时显式传 `stream=True`，或使用 `responses.stream(...)`。流式开关不改变 `reasoning.effort`；`high` 仍原样传给原生后端。请求记录中的“响应类型”保存实际采用的流式/非流式设置，原生 WebSocket 会话记为流式；没有该字段时显示“未知”。

`text.format` 是对象，例如 `{"text":{"format":{"type":"json_object"},"verbosity":"low"}}`。省略 `format` 或传 `null` 时使用普通文本。`text.verbosity` 支持 `low/medium/high`，映射到原生 `model_verbosity`；`service_tier` 支持 `auto/default/flex/priority`，`auto` 沿用原生配置，其余在缓存优化开启时映射到原生 `serviceTierForTurn`，只作用于当前轮，避免改变后续独立请求的默认等级。关闭优化时使用原有 `serviceTier` 映射。这些设置真实传给原生后端，模型及提供方是否支持仍由上游决定；中转站不伪造实际计费等级。

`json_object` 是适配层模式：保留调用方 instructions，追加 JSON 对象输出指令，在收到完整原生消息后用 JSON 解析器校验。它不等于原生 JSON 解码模式，也不使用上游不接受的宽松 strict Schema。只返回合法 JSON 对象，不剥 Markdown、不改写答案；无效结果返回 `UPSTREAM_INVALID_RESPONSE`。SSE 保留事件协议，但正文在校验通过后整段发送，原生 commentary 不作为 JSON 正文发送；真实函数调用照常返回，调用方提交工具结果后再继续。普通文本和 `json_schema` 的流式行为不变。

`max_output_tokens` 自动忽略：携带该参数不会导致请求被拒绝，也不会限制生成长度、截断回答或修改模型请求。返回的 Response 中 `max_output_tokens` 为 `null`。JSON 与 SSE 均采用此行为，实际输出仍由原生后端及上游模型决定。

输入可以是字符串，也可以是 user/assistant/system/developer 消息、function_call、function_call_output 和 reasoning 条目的数组。reasoning 历史支持官方的 `content` 数组（`reasoning_text` 或 `text` 内容项）以及 `summary`/`encrypted_content`；这些字段会随历史回放保留。工具结果的 output 支持字符串或 `input_text` / 内嵌 `input_image` 内容数组；图片默认自动接入，账户可显式关闭 ImageInput，沿用相同容量限制，禁止远程 URL、服务器路径和未适配的 file_id。HTTP 多轮需回传完整历史；同一 WebSocket 连接内也可使用 previous_response_id 加增量输入。顶层 instructions 不自动继承。

Codex 桌面发送的 `{"type":"additional_tools","role":"developer","tools":[...]}` 也可作为 input 条目，保留其历史位置和可选 id。工具定义同时用于注册原生动态工具，不会被当成普通文本或直接丢弃。续接时从完整历史（包括同一 WebSocket 连接内 previous_response_id 引用的历史）恢复工具；后续同名定义覆盖旧定义，namespace 增量仅更新对应成员，未更新成员保留。顶层 tools 不跨请求自动继承。每批定义与最终合并目录均校验，合计最多 1,024 个工具；命名空间工具默认可用，搜索和生图默认自动接入。`tool_choice=none` 禁用动态工具，并在注入原生历史时清空 additional_tools 的工具数组。

app-server 禁止动态工具使用 `functions` 等保留命名空间和 `mcp__` 前缀。HTTP 适配器为这些标识生成稳定的内部别名，同步转换工具定义、历史与回调，在客户端响应中还原原始 name/namespace；不会将客户端工具指向中转站宿主工具。原生仍会在本轮加入它自己的工具上下文，因此不保证上下文逐字节等同于直接 Responses 请求。

Responses HTTP 和 WebSocket 共用 custom 适配：裸工具及 namespace 内均接受省略 format、`format.type=text` 或 `grammar`（lark/regex）。官方 app-server 的 DynamicToolSpec 仅支持 function/namespace，因此内部将 custom 映射为接收 `{"input":"原始文本"}` 的函数，同时转换注入的 custom 调用/结果历史；对客户端恢复 `custom_tool_call`、`custom_tool_call_output` 与 `response.custom_tool_call_input.delta/done` 事件，保留 namespace、call_id 和原始文本。工具仍由调用方执行。grammar 原文作为工具说明传递，**不提供原生 grammar 约束解码保证**。`/v1/native` 的动态工具协议保持独立。

工具映射与历史回放规则见[工具协议与排障](TOOL-PROTOCOL.md)，已核对的官方版本见[桌面接入说明](RESPONSES-COMPATIBILITY.md#容量诊断和验证)。回归覆盖 HTTP JSON/SSE 入口、工具目录合并和连接内续接，以及本机原生进程连接合成上游的 function/custom 工具往返。

工具示例见 [responses_client.py](../examples/responses_client.py)。模型返回标准 `function_call`，由客户端执行，并在下一次请求中提交同 call_id 的 `function_call_output`。中转站不执行这些业务函数，也不伪造工具结果。一次响应中的多个函数调用必须全部提交结果后再继续。

## 原生传输与兼容范围

本节仅描述原生账户的执行与资源回收；API Key 直连不使用这些原生进程和线程缓存。

```text
客户端 HTTP / SSE → 接入适配 → 原生 app-server → 模型服务
```

普通 Responses HTTP/SSE 和 Responses WebSocket 请求在同一账户内共享兼容的 app-server 进程，每个独立对话创建自己的原生 thread，允许多个 thread 同时推理；Key、账户、历史、指令与工具仍按请求校验，分组使用当前授权重新检查。成功请求后释放并发额度，新对话或不同 Key 不再要求重启进程。原生安装信息、代理、生图模式或有效配置变化时使用兼容的新进程；缓存优化开启时，目录内模型切换可沿用同一进程，目标模型不在已净化目录中则仅在准备阶段重建进程；在途请求不因其他对话切换而被终止。原生 `/v1/native` 长会话仍使用独占执行器；原生显式压缩复用账户认证，以 HTTP 在内存中收发上游条目。

共享进程可保留多个临时线程。同一 API Key、账户路由、instructions、工具及线程设置一致，当前分组仍有权使用该账户和模型，且调用方提交的历史完整匹配上一轮输入和输出时，追加 user 消息可沿用空闲线程。优化开启时，已完成轮次之后允许修改 `reasoning.effort`、`reasoning.summary`、`service_tier` 和 JSON Schema，由下一次 `turn/start` 应用；文本与 Schema 可互换，`json_object` 涉及线程指令而需重建。新增 system/developer 消息及推理强度 `configuration_update` 可随 user 消息一起注入，显式请求强度优先于历史设置。显式客户端会话头也参与线程隔离，`prompt_cache_key` 则只参与账户选路。支持同一 WebSocket 内的 `previous_response_id` 和自行回传完整历史；分叉、修改旧历史或改变工具、顶层 instructions、verbosity 等线程设置会建立独立线程，跨 Key 不复用线程。完成轮次后切换模型时，优化开启、显式提供 `reasoning.effort`，且目标模型在进程已净化目录中，才可沿用原线程；工具待答期间切换模型或省略新模型的推理强度时按完整历史重建。官方能力及本项目边界见 [原生会话能力对照](NATIVE-CACHE-CAPABILITIES.md)。

优化开启时，先从原始请求提取逻辑线程、会话和明确父线程身份，再按原规则过滤并映射 `client_metadata` 到原生 turn。逻辑身份参与线程隔离，逐轮追踪元数据不作为线程稳定参数。历史摘要忽略 `internal_chat_message_metadata_passthrough` 中已知的消息追踪和内容来源字段（`turn_id`、`create_time`、`content_item_kinds`、`content_item_metadata`），兼容 Codex 桌面端回传历史时去除这些字段的行为。只调整匹配摘要，不改写发送的历史；正文、消息 phase、加密内容、工具调用与结果以及工具执行元数据仍需匹配，未知历史元数据变化仍回退完整历史。

身份字段的来源优先级为：请求／WebSocket 帧的 `client_metadata` 显式字段、帧内 turn metadata、请求体显式字段、连接级 `x-codex-turn-metadata`、对应请求头。turn metadata 支持 `x-codex-turn-metadata`、`turn_metadata`、`turnMetadata` 字符串；逻辑字段包括 `session_id`、`thread_id`、`parent_thread_id`、`forked_from_thread_id`／`forked_from_id`。请求头支持 `session-id`／`session_id`／`conversation_id`、`thread-id`／`thread_id` 和 `x-codex-parent-thread-id`。帧提供自己的会话身份时，不沿用连接的线程身份；同级重复身份互相冲突时禁用该请求的线程复用。标识最多 512 字节且不能含控制字符，turn metadata 最多 16 KiB，只保存身份摘要。

账户选路和原生线程隔离分别校验。当前逻辑线程 ID 优先，缺失时使用逻辑会话 ID 或会话头；相同缓存选路头不能合并不同逻辑线程。子代理、分支可以首次继承明确父线程的首选账户，但使用独立原生线程和独立账户绑定；缺少独立身份时不领取已有线程或工具回调。父身份、`prompt_cache_key` 和历史前缀都不能授权领取聊天状态。不会根据代理名称、request ID、turn ID 或 `root_turn_id` 推断身份。

亲和绑定在请求成功准入时建立或续期，TTL 默认 24 小时，可在系统设置中调整（1–720 个整数小时）；临时分流保留并续期原账户，响应完成不再写回绑定。仅入队或被拒绝的请求不续期，API Key 直连不使用此机制。软亲和按 Key、模型和对话身份匹配，分组不参与匹配、换组不主动清除绑定；路由权限继续按当前分组检查，原生线程与回调不再因分组 ID 改变而失效。修改时长不立即改写已有绑定的到期时间，新建或下一次续期使用新值。设置持久化，绑定仅保存在内存中，重启后清空。详细调度规则见 [账户亲和](OPERATIONS.md#多账户智能调度)。账户亲和 TTL、工具等待的 5 分钟、进程回收条件及上游 Token 缓存有效期是不同限制，亲和续期不保证缓存命中。

推理条目的 `content` 缺失、`null` 和空数组也按同一种无明文内容的形式计算匹配摘要。官方客户端可能在回传时补上 `content: null`，不应因此重建线程；非空推理内容、摘要和加密内容仍严格比较。此规则只用于优化开启时的线程匹配，不修改响应或注入的历史。

参数摘要按已确认的原生执行语义合并等价表示：`reasoning.effort/summary`、`text.verbosity` 的缺省与 `null` 等价；未提供 instructions 与空字符串等价；未提供 service_tier 与继承账户配置的 `auto` 等价。无显式会话标识时的账户亲和使用线程设置摘要，调整逐轮设置不改变亲和提示。省略之前显式设置的强度时，恢复 `thread/start` 报告的默认值；摘要恢复配置读取结果，未配置时使用官方默认 `auto`。默认值无法确认则重建，避免继承旧请求的覆盖值。不递归删除工具或 Schema 内有意义的 `null`。

工具等待期间允许修改推理强度、摘要和服务等级：通过官方 `features.step_model_switching` 启用 `turn/settings/update`，在新请求准入、领取及整批结果校验完成后更新，收到 `status: applied` 才回答任何回调。服务等级恢复到新建线程报告的默认值；未知默认值需要重建。该更新不改写未来轮次的线程默认值。模型、工具、历史、输出格式、verbosity 等仍严格匹配；更新被拒绝、目标消失或连接异常时清理该 turn，不自动重放。

原生函数及适配后的 custom/namespace 工具可以保留待答回调：向客户端返回完整工具调用响应后释放本次请求并发额度，原生 turn 留在等待状态，最长 5 分钟；资源压力或进程退役可提前结束等待。下一次请求须提供全部最终工具结果；在鉴权、路由和完整历史校验通过并取得新请求额度后，才一次性回复对应回调，沿用原 thread/turn 生成下一次响应。每次客户端请求分别生成 Response ID、事件序号和用量。等待状态只保存摘要、回调身份和资源句柄，不保存一份聊天正文。竞争领取同一待答回调返回 `RESPONSE_CONTINUATION_BUSY`（HTTP 409，流式协议使用错误事件）。

待答状态每账户最多 32 条、全局最多 1,024 条。超时、资源压力、账户停止、撤权或配置失效会中断对应 turn；不能确认清理时退役进程并等待其他活动请求结束。工具目录变化、客户端压缩或替换旧历史、额外控制消息、补充工具输出以及原生回调无法等价表示的图片 detail 使用完整历史重建；一旦发送了工具结果，失败或结果未知都不自动重放。内存状态丢失后仍可在原有响应链绑定校验通过的前提下完整回放。

原生自动压缩完成后可以继续复用同一线程。桥保留客户端完整输入与已交付输出的摘要，并将其与当前原生线程、已确认的压缩检查点一起保存；该摘要用于验证客户端续聊，不要求它等于原生压缩后的模型输入。下一请求仍须完整匹配客户端历史，只追加新消息或回复已核对的工具回调，不重新注入压缩前历史。HTTP JSON、SSE 和 WebSocket 展开后的历史共用这一规则；可跨多次原生自动压缩继续对话，压缩摘要由原生管理，桥不额外保存正文。

压缩开始与完成通知必须属于当前 thread/turn 且条目标识匹配；未确认完成、通知不一致、线程关闭或在途历史被 steer 修改时不保留线程。工具调用交还客户端前已经完成的压缩，以及回复工具结果后原生执行的压缩，均可继续保留同一 thread/turn；待答期间未经新请求领取便出现压缩或新的模型输出时撤销待答状态，领取时也检查事件，避免与后台检查竞态。客户端自行改写、缩短或替换历史仍走完整历史重建；原生显式 `/responses/compact` 与客户端 `compaction_trigger` 使用账户认证的 HTTP 压缩传输，不创建持久线程，返回真实加密条目。原生线程复用不表示压缩前的 Token 缓存原样保留，实际缓存命中仍以上游用量为准。

启动环境变量 `BRIDGE_NATIVE_CACHE_REUSE=0` 可恢复原有的文本续聊亲和及工具交还后中断策略；默认 `1` 启用上述优化。它不改变官方 app-server、不绕过响应链绑定，也不保证上游实际缓存命中。实际效果以返回的 cached tokens 和现有管理台统计为准。

共享 Responses 线程设置 `thread_unload_delay_secs=0`，缓存中的线程保持订阅；资源不足时原子选中空闲线程并取消订阅。仅收到成功确认不代表资源已释放，必须等 `thread/closed` 或通过完整分页的 `thread/loaded/list` 确认不存在。清理总期限 5 秒，无法确认则安全退役进程，等待正在生成的请求完成。正在生成和等待工具结果的线程不参与普通 LRU 淘汰；工具等待的超时、超预算和资源压力取消仍通过原有所有权守卫确认中断。额外进程保留空闲 120 秒回收，常驻进程不因年龄淘汰。

共享进程启动配置（修改后重启生效）：

| 环境变量 | 默认 | 有效范围 |
| --- | --- | --- |
| `BRIDGE_NATIVE_MAX_THREADS` | 64 | 整数 1–256 |
| `BRIDGE_NATIVE_CONTEXT_BUDGET_MIB` | 128 | 整数 32–1024 MiB |

原生缓存优化开启时，非法配置阻止启动，上表预算按每个共享 Responses 进程计算。线程预算包含预留、运行、空闲、工具等待和关闭中的状态；未确认关闭前不返还容量。字节预算包含存活线程实际接纳的新增输入、工具结果和去重输出，不按每轮完整历史重复累计；它不是 Token 上下文窗口或进程实际内存上限。输出使预算超限时不打断模型，完成后回收；单线程超预算不继续缓存。资源充足时不设线程 TTL；资源不足时优先淘汰同一逻辑身份已被后续成功对话取代的空闲线程，其他空闲线程按最近成功使用顺序排序。账户亲和时间不改变该策略。已卸载线程的临时目录保留到进程树退出；累计 256 个待清理目录后轮换。`BRIDGE_NATIVE_CACHE_REUSE=0` 时上述两项配置不生效，使用 32 线程／64 MiB 累计预算及关闭优化时的工具收尾行为。

普通生成线程的缓存只保存在原生进程内存中，桥保存历史摘要用于匹配，不另存一份正文。原生显式压缩原先依赖临时 rollout 导出结果，该路径已移除；不以临时保存正文换取压缩结果，见 [能力对照](NATIVE-CACHE-CAPABILITIES.md)。响应正文不持久化供 API 查询，HTTP 无法按响应 ID 读取/续接；这不禁用普通空闲线程的原生执行缓存，工具等待期单独按上述 5 分钟计算；客户端可自行回传完整历史。进程回收后自动走完整历史路径，不依赖缓存才能完成请求。每轮仍重新鉴权并检查账户身份、路由和配置；相同且已验证的外部令牌不重复登录原生进程。

`scripts/test_account_concurrency.py` 使用已安装 app-server、隔离账户与本地模拟上游，覆盖同 PID 的并发对话、跨 Key 历史隔离、多线程续聊、单请求取消、并发工具回调、轮换时保留在途请求及账户停止。`--thread-reclamation-only` 使用低容量配置验证同一对话较早历史状态的空闲线程优先回收、热点线程保留、超过原累计 32 线程仍复用进程、增量预算和 256 个待清理目录轮换，并扫描隔离数据目录确认正文不落盘。关闭优化时单独验证原累计限制。以上验证不消耗真实模型额度。

HTTP 和原生 WebSocket 推理使用已准备的安装信息，请求前不重复执行程序哈希、发布者签名和应用归档的完整安装校验。完整校验保留在安装检测、账户接入检查和显式诊断中；桌面升级后需重新 prepare 并刷新接入信息。

独立 OAuth 推理进程可以复用该账户目录中最近 24 小时、原生版本匹配且包含请求模型的 `models_cache.json`。桥仅把原始模型定义写入本次临时目录，通过原生 [`model_catalog_json`](https://learn.chatgpt.com/docs/config-file/config-reference) 启动选项装载。模型目录是模型能力及默认配置元数据，不是回答缓存，也不代替每轮认证或改变调用方指定的 `reasoning.effort`。目录缺失、超过上限、版本不匹配、已有自定义目录或原生拒绝装载时回退原路径。显式账户接入检查与诊断继续使用原生模型发现。

原生目录下载包含完整响应体的 5 秒超时；下载失败后可能每轮重复等待。使用目录快照的成功请求结束后，若目录已满 1 小时，桥异步启动短时原生进程刷新目录，每账户最多一个、全局最多两个。首次失败后至少等待 5 分钟，连续第二次失败等待 15 分钟，第三次及以后等待 30 分钟；退避从本次尝试结束时开始计时，成功后清零失败次数。到达时间门槛后仍由后续成功请求触发，不会在空闲时定时下载。退避状态保存在服务内存中，停止账户或重启服务后重置。刷新使用临时原生账户目录，只把已经验证的当前 Access Token 注入进程内存，不复制 RT、不发送模型推理。只有原生实际保存了新的有效目录（下载结果仍要求时间戳在最近 4 分钟内）、账户绑定与配置仍一致时才原子替换缓存，并保留原生的真实时间戳、身份标签与 ETag；RPC 返回成功本身不视为刷新成功。外层等待上限仍为 20 秒，原生单次下载的 5 秒超时不变。刷新失败保留原目录，最多使用 24 小时；持续超时超过上限后仍可能恢复原生同步等待。更换凭据、重新绑定和退出登录会清除旧目录；停止账户、删除账户及服务关闭会取消并回收刷新进程。

服务日志中的 `request_preflight` 按 requestId 记录收到请求到提交 `turn/start` 或工具结果前的各段准备耗时，`measurementEnd` 区分这两个终点，并记录 `nativeProcessId`、`reusedNativeProcess`、`reusedNativeThread`；`native_turn_start_ack` 仅记录新 turn 的原生确认时间。它们不以首个回答 token 为终点，也不能当作上游服务实际收到请求的时间；模拟上游的收包计时见 `native_config` 集成测试的 `synthetic_upstream_received` 输出。`wire_capture` 集成测试另验证连续轮次复用同一原生 WebSocket 连接，且每条消息只到达模型一次。

工具缓存续接验收运行 `python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --check-cache-expiry`：使用安装版 app-server 与合成上游，覆盖并行工具、连续工具轮次、custom/namespace、图片结果、并发领取 409、5 分钟真实到期后完整回放、请求占额释放及独立用量。它同时调用 `scripts/test_native_cache_websocket.mjs` 验证 Responses WebSocket 的 `previous_response_id` 工具往返和下一轮用户消息。抓包断言同一工具链的原生缓存键、会话头及历史前缀稳定，并且模型调用只发生一次；隔离测试目录的 `tool-wire.json` 保存合成请求，`result.json` 保存断言结果。将启动环境变量改为 `BRIDGE_NATIVE_CACHE_REUSE=0` 可运行旧行为对照。模拟用量只能证明统计和协议正确。

桌面端续聊兼容验收运行 `python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --desktop-cache-only`。安装版 app-server 同时充当客户端和桥后的执行器，经隔离中转站访问 localhost 合成模型；真实生成三轮带有不同轮次元数据的请求，验证客户端清理消息来源元数据后仍沿用同一原生线程、缓存键和会话头，模型可见前缀不变且每轮只发送一次。`desktop-wire.json`、`desktop-result.json` 保存合成抓包及断言结果；该验收要求默认启用缓存优化，不消耗真实模型额度。

加上 `--desktop-first-reasoning` 可验证首轮包含推理条目的续聊：由安装版客户端实际完成缺失 `content` 到 `content: null` 的回传转换，要求第二轮立即续用首轮线程，且保留原有加密推理内容。

逐轮参数验收可运行 `python scripts/test_native_cache_audit.py --binary target/debug/codex-relay.exe --cases turn_effort turn_tier settings_reset tool_turn_settings tool_settings_changed`。前两项由官方客户端实际切换强度和服务等级；后两项检查工具链结束后切换、等待工具结果时修改允许参数均能复用。`settings_reset` 使用隔离代理构造“显式参数 → 默认值”的转换，核对上游真正收到的强度和等级，避免仅验证线程 ID。`python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --turn-settings-only` 另外覆盖 JSON Schema、摘要切换及默认值恢复、追加指令和 WebSocket 待答参数更新，检查前缀与缓存身份以及工具结果仅发送一次。

扩大客户端往返检查可运行 `python scripts/test_native_cache_audit.py --binary target/debug/codex-relay.exe`；`--cases` 可选择单项。该检查覆盖文本与分段消息、空/非空推理内容、摘要、function/custom/namespace、并行工具、图片及错误文本结果。参数空值、auto、默认值恢复、工具等待时的参数变化和逻辑身份用例在隔离代理中调整请求；其他用例直接使用安装版客户端发出的历史。检查同时断言回调次数、每请求用量、线程/会话头/缓存键和模型可见前缀。消息密文被客户端移除时应回退，不能为了复用而忽略真实上下文变化。报告保存在 `.cache/native-cache-audit-*/result.json`，失败项返回非零退出码；默认 Rust 测试不代替此检查。

逻辑身份矩阵可使用 `--cases identity_header_rotation identity_thread_change identity_conflict identity_anonymous_child`，检查连接头变化、显式线程切换、冲突身份和无独立身份的子线程。双账户亲和与 WebSocket 逐帧身份验收使用 `python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --affinity-only`，入口与范围见 [本地调度负载验证](OPERATIONS.md#本地调度负载验证)。macOS 将二进制路径替换为实际的 `target/debug/codex-relay`。

真实收益需按相同模型、账户与任务类型对照开关两组的现有统计：对已知缓存用量的请求计算 `Σ cached_input_tokens / Σ input_tokens`，同时比较未缓存输入 Token 总量及请求延迟，并记录未知用量占比。未知缓存用量不按零填充，也不由模拟上游结果推断真实命中率提升；这些验收脚本不消耗真实模型额度。

模型 HTTP/TLS、代理、认证、压缩、重试由原生 app-server 处理；不创建过滤路由，不用中转站 HTTP 库请求模型。HTTP Response ID 与 SSE 事件封装由适配层产生；回答、函数参数、call_id 和用量来自原生事件。函数参数和推理摘要在原生完整条目到达后发出，文本支持增量输出。原生函数之外的宿主工具行为仍受 [原生工具边界](NATIVE-DIRECT.md#工具执行边界) 约束。

原生适配提供标准 Responses **文本与函数工具子集**，不是官方所有能力的等价实现。未支持的可选参数在原生请求解析时自动忽略，不转发、不执行，也不因这些参数返回 400。例如 `temperature`、`top_p`、`background`、`truncation`、`parallel_tool_calls` 以及未知顶层字段。强制/指定 `tool_choice` 回退为 `auto`，`none` 保持禁用工具；function 的 `strict` 被忽略并按原生非严格模式执行。`reasoning`、`text`、已知 `text.format` 和已支持工具定义中的未知选项同样移除，JSON Schema 内容保持原样。

自动忽略仅针对可选控制参数。`model`、`input` 等必需字段，以及已支持参数的类型和值仍需合法；不支持的音频/文件内容、其他工具类型或被明确关闭的能力仍报错，不会悄悄删除用户消息或绕过能力开关。仅提供被忽略的 `prompt`/`conversation` 不能替代 `model`/`input`。返回字段描述实际行为：`background=false`、`truncation=disabled`、`temperature/top_p/max_output_tokens=null`。JSON 与 SSE 采用相同规则。

## 参数能力边界

下表描述**原生账户适配路径**的参数处理；API Key 直连不应用这张表中的忽略规则，由上游校验和执行。原生路径继续使用 app-server 网络通道，不能将“接受参数”理解成“支持全部官方功能”。

| 参数 | 当前行为 |
| --- | --- |
| `model`、`input`、`instructions` | 经鉴权、消息校验和转换交给原生后端；输入类型受本文限制 |
| `reasoning.effort/summary`、`text.verbosity` | 原生配置/轮次参数，已覆盖参数映射测试 |
| `reasoning.context: "all_turns"` | 推理强度采用完整历史中最后一条 `configuration_update.reasoning.effort`，没有更新则使用顶层 `effort`；请求记录与原生轮次参数采用相同解析规则。普通请求仍优先使用显式顶层 `effort` |
| `text.format.type=text/json_schema` | 普通输出 / 原生 `outputSchema`；Schema 最终由上游校验 |
| `text.format.type=json_object` | 适配层指令与 JSON 对象校验；不提供逐 token 正文，不伪造原生 JSON mode |
| `service_tier` | 支持 auto/default/flex/priority/ultrafast；default 使用标准速度、auto 继承原生配置。显式 ultrafast 在发送 turn 或回答工具回调前检查当前原生进程的模型目录；未声明时返回 SERVICE_TIER_UNAVAILABLE，不静默降速。目录声明不代替上游账户授权或实际计费等级 |
| `stream`、`store`、`previous_response_id`、`metadata` | 中转站实现 SSE 和同一 WebSocket 内的历史续接；store=true 被忽略，响应返回 store=false；metadata 仅随响应返回 |
| `tools`、`tool_choice` | function/custom/namespace 及已验收的搜索、生图子集；支持 input.additional_tools；auto/none 生效，其余选择忽略并回退 auto |
| `web_search.search_context_size/filters/user_location/external_web_access` | 映射到独立原生线程：搜索上下文 low/medium/high、最多 100 个域名、approximate 位置及实时访问布尔值；false 使用 cached，默认 live。账户位置策略优先；参数改变会重建线程，避免继承旧限制。生图工具选项仍未映射 |
| `parallel_tool_calls` | 忽略；传入 false 不能强制串行，传入 true 也不保证并行；原生实际调度由其工具系统决定 |
| `include` | 忽略选择列表，不能用它要求额外返回或过滤字段；原生已提供且适配器支持的输出及用于续接的加密推理历史仍保留 |
| `background`、`truncation` | 忽略，实际为 false / disabled，不提供后台任务或自动截断 |
| `stream_options` | 忽略；适配层 SSE 当前不生成混淆填充 |
| `max_output_tokens` | 沿用既有兼容策略：忽略，返回 null；不能限制模型输出 token |
| `temperature`、`top_p`、`top_logprobs` | 忽略，不控制原生采样 |
| `prompt`、`conversation` | 原生适配路径忽略，不加载模板或 Conversations |
| `context_management` | 原生适配路径忽略；API Key 直连路径保留并交给上游处理，见下文 |
| `prompt_cache_key` | 优化开启时仅作为本地账户亲和提示；不透传为原生缓存键，不控制上游缓存。关闭优化时忽略 |
| `prompt_cache_retention` | 忽略，不能控制原生或上游保留策略；不代表关闭缓存 |
| `safety_identifier`、`user` | 忽略，不更改原生安全标识 |
| `client_metadata` | 优化开启时先提取本地会话、线程和父线程身份；仅将 `x-codex-turn-metadata` JSON 字符串里的非保留字符串字段映射到原生轮次元数据，其余不按原值转发 |
| `max_tool_calls`、function `strict` | 忽略；函数实际按非严格模式执行 |
| 未知顶层参数、已支持配置对象中的未知选项 | 忽略；不影响保留的参数、消息、metadata 或 JSON Schema 内容 |
| 音频、文件、远程图片、其他托管工具 | 未建立对应处理及验收链路，拒绝 |

`python scripts/probe_native_response_options.py` 在独立原生进程和临时配置中连接本机模拟上游，记录实际发送的参数到 `.cache/native-response-options.json`，不读取真实凭据、不调用真实模型。`native_responses` 集成测试进一步覆盖 JSON 对象、verbosity/service tier 和工具图片数组的实际原生链路。

`python scripts/probe_native_search_tiers.py` 使用隔离的官方 app-server、合成凭据和 localhost 上游，核对实际搜索请求中的域名、位置、上下文大小及实时访问设置，并检查下一个独立线程恢复默认值。测试目录专门声明 Ultrafast，以验证等级传递；不证明真实账户拥有权限或速度提升。成功运行产生 4 次合成模型请求和 2 次合成搜索请求，报告位于 `.cache/native-search-tiers.json`。

管理 API 的账户能力目录保留模型 `serviceTiers` 和 `defaultServiceTier`，未知元数据返回 null，不虚构默认等级。前端类型由 `web/contracts/admin.ts` 推导，并由 `pnpm test:contracts` 验证真实 Rust 响应。调用方 Key 的强制快速模式仍按既有策略覆盖为 priority。

工具结果支持可选 name/namespace，并校验其与对应 call_id 的身份一致。官方 code-mode 的具名 functions.exec notify() 补充输出按顺序保留，既不提前完成调用，也不重新打开已经完成的调用；普通最终结果仍须唯一匹配。

原生请求详情的 parameterHandling 只记录字段路径、处理方式及原因，不记录参数值，也不向标准 SSE 插入警告事件。`client_metadata` 只提取 `x-codex-turn-metadata` JSON 字符串中的非保留字符串字段；session_id/thread_id/turn_id/window_id 由原生生成。`access_programs.cyber` 映射到原生 cyberAccessProgram；原生只向 OpenAI 提供方发送，原生进程自身使用 API Key 认证时还受其功能开关约束。此处指原生进程的认证方式，不是管理台 API Key 直连账户；授权是否可用由上游决定。

### 如何理解参数兼容说明

管理台的“参数兼容说明（N 项）”展示原生适配记录到的参数处理差异，N 是说明条目数，不是失败次数。“已忽略”表示该参数不能控制下游行为；“适配”表示通过转换实现部分语义，不保证与直接调用完全一致。这些说明本身不表示请求失败，也不保证请求成功，需结合请求状态、错误码和客户端工具结果判断。以下解释针对原生路径；API Key 直连没有这些原生过滤提示，也不能据此推断上游一定支持所有字段。

- **附加输出与续聊**：`include` 被忽略不等于删除推理历史；适配器保留原生返回的受支持条目，但不会因为 include 中指定了某项就补出原生未提供的数据。普通 HTTP 续聊仍需客户端回传历史。
- **工具并行**：不能依靠 `parallel_tool_calls=false` 阻止一轮返回多个工具调用。Responses 响应目前固定返回 `parallel_tool_calls=true`，该值不是本轮发生并行执行的观测结果。需要工具按顺序执行时，调用方还应控制自己的执行顺序。
- **缓存与速度**：`prompt_cache_key` 仅用于本地选路，不控制上游缓存键；关闭优化后忽略它也不代表每次都重新计算全部输入。实际命中以原生报告的缓存输入 Token 为准，可查看管理台缓存用量或响应的 `usage.input_tokens_details.cached_tokens`；未报告时为未知，不能按零推断。
- **元数据与追踪**：`client_metadata` 的其他顶层字段、非字符串值及保留的身份字段不按原值转发，可能影响依赖这些字段的追踪关联。中转站本地识别子代理标记是独立逻辑，可识别的字段范围不等于转发范围，见[子代理请求标记](OPERATIONS.md#子代理请求标记)。
- **custom 工具与 grammar**：工具仍可调用，原始输入文本交给客户端执行；grammar 仅作为工具说明，不提供约束解码。客户端仍需处理格式或解析失败，不能把出现该提示当作已经发生工具错误。所有 custom 声明都会记录这项适配说明，即使没有设置 grammar。

例如 `input[0].tools[0].tools[0]` 和 `input[16].tools[0].tools[0]` 表示请求历史中两个位置上的工具声明（索引从 0 开始），可来自 `additional_tools` 或 `tool_search_output`。相同工具在历史中多次声明时可以出现多项说明；路径本身不表示消息被丢弃或工具执行失败。

实现依据：[参数诊断与过滤](../crates/relay/src/responses/compatibility.rs)、[custom 工具转换](../crates/relay/src/responses/tool_catalog.rs)、[响应字段与事件](../crates/relay/src/responses/events.rs)。

### context_management 与直接转发

官方 `context_management` 的 `compaction` 条目用于在正常 Responses 生成中按 `compact_threshold` 触发上游压缩，输出包含供后续会话使用的不透明压缩条目，不需要客户端另行调用 `/responses/compact`。语义以[官方压缩说明](https://developers.openai.com/api/docs/guides/compaction)为准。

| 账户执行路径 | 当前行为 | 能否保留上游参数语义 |
| --- | --- | --- |
| API Key 直连 | 在原生 `Request::parse` 之前分流；HTTP/SSE、上游 WS 优先及下游 WebSocket 桥接均保留 `context_management`，压缩条目及扩展字段随上游响应返回 | 可以传递该语义；实际阈值计数、触发和压缩由上游实现，仍需上游支持 |
| ChatGPT/OAuth 等原生账户 | 通过官方 app-server RPC 执行，当前忽略该字段；原生自动压缩由自身配置控制，显式 compact 暂不支持 | 尚无等价参数映射，不能把原生 `model_auto_compact_token_limit` 宣称为该 API 参数的完整支持 |

需要上游原生语义时，可在“添加账户 → API Key”配置支持该参数的 Responses 服务，并为调用方使用仅包含相应 API Key 账户的分组。混合分组的首次请求仍可能选中原生账户，不能仅靠请求带有 `context_management` 保证走直连。例：

```json
{
  "model": "<上游支持的模型>",
  "input": "<消息或完整历史>",
  "store": false,
  "context_management": [{"type": "compaction", "compact_threshold": 200000}]
}
```

`200000` 只是示例，不是所有模型的推荐阈值。完整历史续聊应原样保留上游压缩条目；响应 ID 和加密历史仍受调用方 Key、分组、账户、模型及提供方绑定约束。API Key 路径不把压缩结果转换成原生函数工具，也不在中转站估算该阈值。直连指模型请求由中转站 HTTP/WebSocket 客户端发送，不能再称其网络连接由官方 app-server 持有。

原生映射复验入口是 `python scripts/probe_native_context_management.py`：使用隔离配置和模拟上游，对比直接配置与原生压缩阈值。已核对版本中，直接配置不进入上游请求；原生阈值仅在本用例第二轮收到上一轮用量后触发压缩，首次注入的长历史不触发；压缩条目写入原生自有历史，却不作为 `rawResponseItem/completed` 返回。因此只加配置映射不能实现等价支持。报告写入 `.cache/native-context-management-probe.json`，无需真实凭据或模型用量。

原生自动压缩后的线程续用验收使用 `python scripts/test_account_concurrency.py --binary target/debug/codex-relay.exe --compaction-only`；加上 `--compaction-position after` 验证轮次结束后的压缩。安装版 app-server 连接本地模拟上游，验证多次压缩后的 JSON/SSE 和 WebSocket 续聊、并行工具回调、线程及缓存身份、压缩后前缀、历史编辑和跨 Key／逻辑线程隔离。配置阈值仅用于隔离测试触发原生压缩，不是对外 `context_management` 参数映射，也不使用模拟缓存数值宣称真实命中率改善。

ChatGPT/OAuth 直连的评估结论：可以复用现有 API Key 转发的调度、传输、响应绑定和统计框架，以及图片直连中读取选中账户凭据、代理和请求头的逻辑；但这不是已支持的 Responses 账户模式。接入前须单独实现认证生命周期和刷新、明确端点与必要请求头、连接隔离及撤权，并确认目标上游确实接受该参数。公开 Responses 文档或图片接口成功均不能证明 ChatGPT 后端支持它。不能把 ChatGPT 访问令牌当成公共 API Key，不能在请求已经发出后静默改走另一执行路径。

`api_key_upstreams` 回归覆盖参数保留、JSON/SSE/WS 压缩条目及扩展字段返回、压缩历史回放。这证明中转链路保留上游结果，不证明真实提供方按阈值执行了压缩；后者需对选定账户和模型另行验收。

## 保存、容量与生命周期

两条路径默认只持久化响应归属、加密条目指纹与请求统计，不保存完整响应正文。启用[请求原文调试](DEVELOPMENT-CHECKS.md#请求原文调试)后，新请求的客户端正文及响应内容会限长保存到 SQLite 请求记录；这些记录供管理员排查，不用于 Responses 查询或续聊恢复。失败诊断记录的是脱敏、限长且省略请求原文的快照，不能作为会话正文恢复来源。API Key 直连的 store、查询/删除和续接行为见[API Key 直连行为](#api-key-直连行为)，其 HTTP JSON 响应累计读取和 SSE 待解析缓冲上限均为 64 MiB，上游 WS 消息/帧上限为 32 MiB；入口请求正文限制仍适用。

**以下保存、缓存和原生历史预算规则仅针对原生适配路径。** `store` 省略、null、false 或 true 均正常处理，响应始终返回 `store:false`；显式 true 的保存要求被忽略。参数处理记录仅标明该选项被忽略，不记录正文。HTTP 查询/删除响应返回 404，HTTP 按 previous_response_id 续接也返回 404；客户端应保留并回传完整历史。

同一 WebSocket 连接内仍支持 previous_response_id 增量续聊。正文仅在该连接的内存缓存及原生执行线程中临时保留，连接缓存关闭后释放；断线重连或服务重启后应省略 previous_response_id 并完整回放。原生线程缓存的独立生命周期见上文。

完整普通文字历史不因消息 ID 而强制绑定账户；原生账户使用软亲和维持选路，原账户不可用时可重新调度。仅 ID 引用、工具条目和加密条目继续使用其已有有效绑定，原生线程和工具回调仍须通过完整归属及上下文校验。

SQLite 中的续聊绑定只保存条目指纹及 Key/分组/账户/提供方/模型绑定和有效期，不保存原始加密内容、输入消息或模型回复；请求原文调试另按上文开关控制。绑定采用 30 天滑动保留，只有通过鉴权和路由校验的有效续聊才续期；支持服务重启后为客户端完整回放的历史选择原账户。当前 Key、分组及模型下查不到有效绑定时（例如账户重新导入、绑定过期或历史来自其他调用作用域），完整历史按新请求正常调度，由上游判断是否接受其中的加密内容；成功或有效的不完整响应会为已接受的加密输入及输出建立当前作用域的绑定。持久化绑定不跨 Key、分组或模型使用；存活的原生线程、工具回调及连接内存历史可在同一 Key 改分组后继续使用，但必须通过当前账户及模型授权检查，也不会在请求发出后自动切换账户重放。已有有效绑定仍固定原账户；同一作用域的历史混合了不同账户或路由、或原路由配置已改变时返回 `RESPONSE_CHAIN_MISMATCH`。该回退需要客户端提交完整历史，不改变 `previous_response_id` 及工具回调的归属校验。

绑定元数据的逻辑预算默认 1 GiB，通过 `BRIDGE_RESPONSE_BINDING_BYTES` 设置（至少 1 MiB）；不再读取旧环境变量名。达到上限先清理过期绑定，仍不足时跳过本次响应绑定保存，保留已经完成的模型回复；已有绑定不被部分覆盖。依赖新响应 ID 的查询或续接可能不可用，客户端应保留完整历史。鉴权、账户或路由冲突及真实数据库读写错误仍会报错；搜索在发送前建立引用会话绑定的容量检查不变。该预算仅统计绑定元数据，不含 SQLite 空闲页、索引和 WAL。`/api/operations` 的 responseBindings 显示绑定数量及用量，bodiesStored 为 false。

数据库不创建正文历史表。显式删除账户或密钥时清理当前绑定与连接缓存；程序不自动重建数据库。

连接缓存最多 128 条 / 128 MiB，单条响应与历史合计最多 96 MiB，TTL 1 小时；优先淘汰旧记录并保护各流最新续接点。新响应超限或没有足够可淘汰记录时跳过缓存，保留已经完成的回复；无法按该响应 ID 续接时，客户端应完整回放历史。撤销 Key 或删除账户会同步失效相关连接缓存及绑定。

正文、解压后正文、合并历史及累计输出默认各 32 MiB，历史最多 16,384 项、工具目录最多 1,024 个。HTTP 支持 gzip/zstd。原生 RPC 仍限 8 MiB，历史分批注入，单项或工具目录无法装入原生请求时明确报错。单请求最长 30 分钟；断开、撤权和停止实例回收拥有的进程树及临时数据。

本机配置实例与已启用接入的 OAuth 账户使用相同 Responses 适配，OAuth 接入方式见 [OAuth 账户](OAUTH-ACCOUNTS.md)。

协议依据：[官方 Responses 流式事件](https://developers.openai.com/api/reference/resources/responses/streaming-events)、[函数调用](https://developers.openai.com/api/docs/guides/function-calling)。

## 可选图片与扩展工具

本节的工具转换、图片格式与额度限制描述原生适配路径。API Key 直连仍受账户关闭项和入口容量限制，但具体输入格式、工具选项及输出由上游处理，不套用原生图片解码和工具选项剥离规则。

客户端 function/custom/namespace/MCP 工具默认可用；图片、搜索、生图默认自动接入。配置好账户路由和调用方 Key 后即可请求，不需要专门验证或逐项启用。新鲜能力目录中声明支持的账户优先选用；目录未知、过期、原生升级、切换模型或服务重启均不构成拒绝请求的理由。实际不支持时返回原生程序或上游模型的真实失败，不伪造成功，也不默默移除能力。

“账户设置 → 高级能力 → 能力与原生版本”可手动关闭图片、搜索或生图。管理 API：

- `POST /api/accounts/{id}/features`，`{revision, disabledFeatures:["imageInput"]}`，只关闭列出的可选能力；传空数组恢复全部自动接入。namespace 客户端工具属于基础能力，不能通过此开关关闭。
- 旧的 `{revision, features:[...]}` 写入方式不再接受。普通 Responses API 请求无需这些管理参数。
- `POST /api/accounts/{id}/features/verify`，`{feature:"imageInput"}`，仅在主动点击“运行诊断（可选）”时发送真实模型请求，消耗实际用量。诊断成功或失败均不修改能力开关。接口也接受 namespacedTools、webSearch、imageGeneration；诊断证据保留模型、安装版本和时间。

能力策略只接受 disabledFeatures，明确列出关闭的图片、搜索或生图能力；features 仅作为响应中的有效能力列表，不再接受旧允许列表写入。刷新目录、重启和诊断不会重新开启关闭项。Key 的模型授权、分组隔离、账户启停、额度、并发及历史绑定继续生效。

图片使用 user 消息中的 `{"type":"input_image","image_url":"data:image/png;base64,..."}`。支持 PNG、JPEG、WebP，完整解码校验；单张最多 4 MiB、合并历史最多 32 张/16 MiB，尺寸与解码内存有界。不接收服务器文件路径或远程 URL。正文与完整历史各最多 32 MiB，媒体同时受上述预算限制。在途 steer 只接受 user 文本，图片应放在下一次 response.create。

namespace 工具格式为 `{"type":"namespace","name":"client","description":"...","tools":[{"type":"function","name":"echo","parameters":{"type":"object"},"strict":false}]}`。返回的 function_call 保留 namespace、name 和 call_id，调用方执行真实函数后回传 function_call_output。

搜索/生图分别使用 `{"type":"web_search"}`、`{"type":"image_generation"}`；额外工具参数自动忽略。输出保留原生 web_search_call / image_generation_call、引用与 base64 结果。媒体临时文件位于请求专属目录，回收原生进程后删除；不开放本地文件下载接口。桌面 `image_gen` 使用的独立 Images 接口见 [图片接口](IMAGES.md)；桌面独立搜索见下节。文件、音视频、托管 MCP 与 file_search 不在此适配范围。

### 桌面独立搜索

`POST /v1/alpha/search` 接受桌面原生 Search JSON：必需 `id`、`model`，以及 `commands` 对象或 `input` 上下文。原生账户的兼容请求通过官方会话调用 `web.run`，由原版程序发送搜索 HTTP。独立 reasoning/max_output_tokens、不兼容 settings 和扩展字段在首请求发送前选择直连；API Key 账户始终直连。

官方工具结果从原生工具输出提取 `output`、`results` 和引用，`transport=official-app-server-tool-turn`；不伪造原生未暴露的 `encrypted_output`。它增加文本模型调用，usage 统计原生文本推理。直连保留原始 `encrypted_output` 和未知字段。响应头 `x-codex-relay-upstream-transport` 标明 native-tools/direct-http。同一 id 的账户和传输模式在首个上游请求前固定，已执行失败不换路径重试。

官方搜索会话按 Key、分组、授权模型、账户、代理、参数及完整上下文隔离；同一会话串行执行，完整上下文须相同或追加。原生进程空闲 120 秒、轮次/上下文预算耗尽、槽位被其他请求回收或重启后，旧会话返回 SEARCH_SESSION_EXPIRED，调用方须以新 id 重新搜索。不能把旧引用转交另一原生会话。直连引用生命周期仍由上游决定。请求上限 32 MiB，官方 RPC 输入另受 8 MiB 消息上限限制；超限在首请求前选择直连。结果上限 64 MiB，等待上限 300 秒。

搜索会话 `id` 按 Key 和分组绑定账户、模型及提供方版本；后续引用继续使用原账户，不能静默切换账户。更换模型应使用新 `id`。数据库仅保存 ID 摘要和路由绑定，不保存搜索上下文与查询正文。Key 撤销或更换分组沿用原有绑定清理机制。

请求记录显示“网页搜索”，包括成功、上游失败、输入拒绝、取消及交付状态；错误详情保留 HTTP 状态与脱敏错误。已认证的未知 `/v1/*` 路径和错误方法显示“接口拒绝”，返回结构化 404/405 与 `x-request-id`，不再落入管理台静态文件处理器。无效 Key 仍只返回认证错误，不建立归属不明的业务记录。

搜索回归位于 `api_key_upstreams` 的 `standalone_search_*` 用例与 `web/tests/search-records.spec.ts`。`python scripts/test_images_api.py` 默认连接本地模拟上游，额外验证原生认证下的搜索、引用续接和失败记录，同时回归共用传输上的图片能力；不会发送真实搜索或真实生图。macOS 真实上游与便携包仍需在目标环境验收，不能用本地模拟通过代替。

`GET /v1/usage` 提供当前 Key 自助统计。`X-Request-ID` 可与管理界面的请求详情对应。仅当所需能力被可用账户的手动策略关闭时，调度返回 `CAPABILITY_UNAVAILABLE` 并列出能力及处理办法；没有启用路由返回 `INFERENCE_DISABLED`。不会因为缺少验证记录而拒绝，也不会静默改成纯文本请求。

工具待答期间允许在完整工具结果批次中附带合法的 `configuration_update`。配置项保留在历史中，最后一次强度设置须先通过原生 `turn/settings/update` 确认，再回答工具回调；缺失、重复或不匹配的工具结果仍拒绝续接。能力准入只检查工具声明与消息/工具结果中的媒体项，不扫描 JSON Schema、metadata 或函数参数中的示例数据。
