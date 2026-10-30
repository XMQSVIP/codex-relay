# 原生直连会话

原生会话入口是 `/v1/native` WebSocket。标准 SDK 可使用 [Responses HTTP/SSE 入口](RESPONSES.md)。连接使用 `Authorization: Bearer <API Key>`，网络入口的 Host/Origin 校验、Key 分组/模型授权、队列及并发设置继续生效。可选择本机配置实例或已配置接入的 OAuth 账户；桌面登录复用也在独立推理进程中运行。

```text
客户端 ↔ 中转站 WebSocket/RPC ↔ 官方 app-server ↔ 模型服务
```

上游 HTTP/TLS/模型 WebSocket、代理、认证、压缩与重试由官方 app-server 自己处理。中转站不会为这些会话创建过滤路由，不覆盖提供方 base_url，不使用 reqwest 请求模型。客户端与中转站之间的 WS/HTTP 连接属于接入侧，不是模型传输侧。

## 接入

本机地址 `ws://127.0.0.1:4317/v1/native`。其他网络入口将已配置 Base URL 的 http/https 替换成 ws/wss，再追加 `/native`。反向代理需支持 WebSocket Upgrade。

连接后 15 秒内发送：

```json
{"id":1,"method":"thread/start","params":{"model":"<模型>","dynamicTools":[{"type":"function","name":"client_time","description":"Read caller time","inputSchema":{"type":"object","properties":{}}}]}}
```

返回 result 中包含原生 thread ID。默认支持 function 和 namespace 动态工具，无需能力验证；此原生入口不接受 custom 或任意服务器配置。可选 developerInstructions 由原生线程接口传入。

开始一轮（以下为文本输入，图片输入需先启用相应能力）：

```json
{"id":2,"method":"turn/start","params":{"input":[{"type":"text","text":"调用 client_time","text_elements":[]}]}}
```

接收原生通知以及带 id 的 `item/tool/call`。工具参数位于 params.arguments；在客户端按已注册工具名单执行，然后用**该回调的 JSON-RPC id**回复，不使用 callId 替代：

```json
{"id":0,"result":{"success":true,"contentItems":[{"type":"inputText","text":"2026-10-23T12:00:00Z"}]}}
```

原生后端接收结果后自行继续模型推理。`turn/completed` 表示本轮结束，后续可继续发送 turn/start，历史由同一原生线程维护。不要将整个历史重复放入每次文本输入。

- `turn/interrupt`：中断当前轮次。
- `session/close`：结束整个会话，回收独立进程并结算统计。
- 断开连接、管理员停止实例、服务关闭会结束会话。会话最长 30 分钟；连接期间占用一个 Key/实例并发额度，包括等待客户端工具结果和轮次间空闲时间。
- 授权与路由每 10 秒复核一次；暂停、撤销、分组变更可结束现有会话。
- 参数错误、未知方法、跨会话工具回复或重复回复会关闭会话。不能调用 account/config/fs/command 等任意原生管理接口。
- 原生通知保持原结构，不模拟 Responses SSE 或完整 Response JSON；真实原生 ID、工具回调和模型事件均由原生后端生成。

## 工具执行边界

客户端注册的业务函数通过 `item/tool/call` 回调到客户端执行。已关闭 shell、浏览器、应用、插件等主要宿主能力，并使用只读沙箱、untrusted 审批策略；未开放的回调自动拒绝。

当前安装版本仍保留代码编排和资源枚举等原生内置能力，部分功能开关无法完全移除。因此不能宣称服务器完全不执行任何原生工具代码；动态业务函数由客户端执行与原生内部编排是两个不同边界。不支持将这个入口当作对任意不可信调用方的完备隔离沙箱。

“原生传输”表示模型连接由原生可执行文件持有，不代表与个人桌面任务的提示词、工具集合、网络指纹或会话完全相同。除本机配置路由外，独立 OAuth 账户配置接入后也可使用该协议；凭据导入与桌面复用见 [OAuth 账户](OAUTH-ACCOUNTS.md)。

## 示例和验收

```powershell
$env:BRIDGE_NATIVE_URL = "ws://127.0.0.1:4317/v1/native"
$env:BRIDGE_API_KEY = "<调用方密钥>"
$env:BRIDGE_MODEL = "<已启用模型>"
node examples/native_client.mjs
```

示例依赖项目已有的 ws 包，会产生真实模型请求。

`native_direct` 集成测试使用独立目录、合成凭据与本地合成模型，核验每条上游连接的 Windows PID 为官方 app-server，并验证客户端工具回传与同一线程第二轮：

```powershell
$env:BRIDGE_NATIVE_INSTALLATION = (Resolve-Path '.cache/launch-config.json').Path
node scripts/cargo-with-memory.mjs test --locked --test native_direct -- --ignored --test-threads=1
```

协议参考：[官方 App Server 文档](https://learn.chatgpt.com/docs/app-server)。以本机版本导出 schema 为准。

## 可选输入与工具扩展

连接开始的 thread/start 可以声明 `capabilities:["imageInput","namespacedTools"]`，网关默认允许这些能力，并优先选择目录已声明支持的路由。无需验证记录，账户明确关闭 imageInput 时排除该路由。声明后才可在 turn/start 使用 `{"type":"image","url":"data:image/png;base64,..."}`。图片大小、类型和解码限制与 Responses 相同；不允许 localImage/localAudio、服务器文件路径或任意远程下载。

`dynamicTools` 支持原生 namespace：`{"type":"namespace","name":"client","description":"...","tools":[{"type":"function","name":"echo","description":"...","inputSchema":{"type":"object"}}]}`。工具回调保留 namespace，按原生 JSON-RPC id 回复。此原生入口的服务端搜索/生图尚未开放，请使用 Responses 工具入口。

## 原生会话生命周期

连接先校验 Key、Host/Origin、模型权限和起始消息，再取得当前分组、Key 与账户的准入额度。每个连接拥有独立原生进程和临时线程；提供方配置复核后才发送 turn，客户端只能回答本连接正在等待的工具回调。真实 thread、turn、item 和回调 ID 保留原值，用量来自原生通知。

准入租约覆盖整个连接，包括工具等待与轮次间空闲。传输使用有界队列、消息大小限制及发送超时；事件丢失、未知方法、跨会话或重复工具回复会结束会话。关闭、取消、超时或权限失效时中断轮次，回收所属进程树，结算后释放额度。HTTP/SSE 的请求级生命周期见 [Responses 接入](RESPONSES.md)。

## 客户端身份与证明

原生 `clientInfo` 取自已校验安装，模型侧 User-Agent 和客户端标识由原生程序生成。协议适配和本地抓包不能证明中转站与桌面 GUI 的提示词、工具、网络指纹或设备身份完全相同。

`Rpc::set_attestation_provider` 可在初始化前注册私有证明回调；普通后端未注册真实提供方，声明 `requestAttestation: false`。已注册时，`attestation/generate` 只接受空参数，同一连接最多一个待处理请求，8 秒超时，返回值须为非空、最多 64 KiB 且可作为 HTTP 头的令牌。令牌仅回到原生 RPC，不进入客户端事件流；回调不验证签名或服务端接受状态。协议探测入口见 [开发检查](DEVELOPMENT-CHECKS.md#协议探测与实验)，协议依据为 [官方 app-server 文档](https://learn.chatgpt.com/docs/app-server)。
