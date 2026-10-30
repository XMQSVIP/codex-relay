# 工具协议与排障

本文说明当前工具身份、历史回放及失败诊断规则。沿用未经修改的官方二进制，模型网络连接由官方进程持有。参数是否被忽略或适配，见[如何理解参数兼容说明](RESPONSES.md#如何理解参数兼容说明)。

## 当前行为与排查方向

Responses 原生工作进程使用派生的模型目录，将模型自带的 `tool_mode` 固定为 `direct`，关闭模型自带的协作、补丁和实验工具声明，并关闭请求输入等内置工具。原生模型目录的工具模式优先于 `features.code_mode`；单独设置该 feature 为 false 不能防止 `exec` 出现在模型请求中。派生目录只写入工作进程临时目录，不修改账号缓存、全局配置、上下文长度、推理等级或图像能力；客户端显式声明的 custom/function 工具仍按原有映射注册。

优先使用有效原生缓存；无缓存或配置了自定义目录时，由同一个原生程序的 `debug models` 导出有效目录，再派生工具策略。导出失败、目录无效或请求模型缺失时在推理前报错，不降级到带宿主工具的默认目录。此路径要求原生版本支持 `debug models` 和 `model_catalog_json`。隔离回归 `native_responses` 检查实际上游声明，覆盖只提供 `content_feedback`、无参数调用与结果续接，以及 `tool_choice=none`，而不只是预设模型返回的工具名。

| 场景 | 当前处理与排查方向 |
| --- | --- |
| 已声明 function 返回原始名称 | 原生别名优先；允许精确匹配已声明的原始身份，未知工具和错误 namespace 仍拒绝。遇到 `NATIVE_TOOL_SCOPE` 时核对本轮目录与错误中的 name/namespace。 |
| 已声明 `functions.exec`，模型调用却省略 namespace | 原生别名和原始身份精确匹配失败后，仅为无 namespace 的模型输出补匹配已声明的默认 `functions` 成员；不猜测其他命名空间，不放宽原生回调校验。裸工具同名时保留原有优先级。 |
| custom/tool_search 历史转换与条目 ID | 转成 function 时移除原类型的可选条目 `id`，保留关联调用、进度通知和最终结果的 `call_id`；未改变类型的条目继续保留 ID。上游若报告 `ctco_…` 等 ID 前缀与条目类型不匹配，检查实际运行版本和转换后的历史。 |
| 桌面显示“原生请求未完成” | 结合 request ID 检查具体错误原因及有界工具标识；`UPSTREAM_REQUEST_FAILED` 需结合对应原生日志定位。工具校验诊断不含参数、代码或会话正文，不能仅凭通用错误码猜测失败工具。 |
| 裸工具历史中的默认 `functions` | 统一识别默认命名空间，历史调用与工具目录使用同一映射。 |
| 只提供 namespace 的工具结果 | 按 call_id 找到调用身份，转换 namespace；无 name 的最终结果继续保持无 name，避免误当作 notify。 |
| 模型返回格式错误的参数后，客户端报告错误并续聊 | arguments 按原始字符串保存和回放，解析及业务执行归客户端；非字符串仍拒绝。 |
| 重复、空或过长 call_id | 在输出调用前校验，跨 function/custom/client tool_search 共用本轮调用 ID 集合。 |
| tool_choice=none 时返回工具调用 | 包括原生 tool_search 在内，均在输出前拒绝。 |

## 自动验证范围

- `tool_identity_matrix_roundtrips_declarations_calls_and_results`：288 组历史映射组合，覆盖 function/custom、裸工具/显式 namespace、MCP/保留名称、defer_loading、默认 functions，以及结果的 name/namespace 字段组合。
- `omitted_functions_namespace_restores_desktop_exec_and_history`：覆盖省略/null namespace 的原生 custom、JSON 包装 custom 和普通 function 调用，验证 JSON/SSE 输出身份、原始代码和结果续接。`omitted_default_namespace_does_not_relax_callback_or_other_namespace_scope` 验证命名冲突优先级及错误命名空间拒绝；SDK 集成回归同时覆盖无 namespace 的 custom 调用及续接。
- `converted_tool_history_drops_typed_item_ids_but_preserves_call_links`：覆盖带 ID 的 custom 调用、进度通知、媒体结果、client tool_search 调用与结果，验证仅移除改变类型的条目 ID，保留原始历史、call_id、内容和未转换条目的 ID。SDK 模拟上游对 function 调用及结果的 ID 前缀执行 HTTP 400 拒绝，JSON/SSE/WS 续接携带真实形式的 `ctco_` 结果 ID，工具搜索历史另行覆盖 JSON/SSE。
- `gateway_evolution`：具体错误原因、清理错误优先级、诊断内容不进入序列化统计摘要。
- `scripts/test_responses_sdk.py`：Python/JavaScript SDK、JSON/SSE/WS、原始名称、结果续接、错误参数恢复、失败后同连接继续；并保留双官方程序、图片、notify、并行工具、压缩、中断和撤权回归。
- `native_responses`：隔离合成凭据与模拟模型，校验模型连接进程归属，覆盖命名空间、MCP、custom、工具搜索和客户端执行边界。

这些测试覆盖协议和适配行为，不能代替真实桌面 GUI、真实模型及全部插件生态验收。常规验证命令见[桌面接入说明](RESPONSES-COMPATIBILITY.md#容量诊断和验证)。

真实工具结果续接可使用 `python scripts/test_real_tool_roundtrip.py --real-upstream` 单独检查。该脚本在隔离实例发起真实模型请求，验证 `functions.exec` 自定义调用、带 `ctco_` ID 的结果回传及后续回答，会产生真实模型用量。脚本仅解释预先约定的固定回显，不执行任意模型代码，不修改运行中的中转站或现有账号数据；真实上游检查需要显式传入 `--real-upstream`。此检查覆盖工具结果续接，不等于所有桌面功能验收。

## 原始工具声明与执行权

通过 `thread/inject_items.additional_tools` 注入原始声明可以让模型看到工具，但不会自动把执行权交回桌面或 SDK。原生后端可能执行工具、写入结果并继续生成；观测 raw 事件或挂接 hook 不等于暂停派发。当前适配采用 `dynamicTools` 和实际注册的 `item/tool/call` 回调，并以别名避开保留名称，再映射回调用方的声明身份。

该边界的源码依据为 `adca78ddae79568279d0731dfdebef1df16f3cde` 的 [动态工具校验](https://github.com/openai/codex/blob/adca78ddae79568279d0731dfdebef1df16f3cde/codex-rs/app-server/src/request_processors/thread_processor.rs#L311) 与 [工具派发](https://github.com/openai/codex/blob/adca78ddae79568279d0731dfdebef1df16f3cde/codex-rs/core/src/stream_events_utils.rs#L316)。`thread/sample` 返回的文本与用量不能代替完整 Responses 工具条目；复制 thread/turn RPC 也不能恢复逐字节相同的桌面 GUI 会话。复验入口见 [协议探测](DEVELOPMENT-CHECKS.md#协议探测与实验)。
