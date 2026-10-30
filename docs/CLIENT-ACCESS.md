# 调用方密钥与接入

## API Key 上游直连

分组可包含独立 API Key 账户。客户端仍使用中转站签发的 API Key；中转站调度到此类账户后，用该账户保存的上游密钥直接访问配置的 API 前缀，全程绕过 app-server。

支持 Responses HTTP JSON/SSE 和 WebSocket、`/responses/compact`、`/responses/{id}` 查询与删除、`/models`、`/images/generations` 以及 JSON/multipart 图片编辑。上游扩展参数、工具定义、响应字段和 HTTP 错误直接保留；不增加 Chat Completions，也不把 API Key 账户用于 `/v1/native`。

`store`、`background`、`previous_response_id` 由上游处理。默认仅为续接保存响应 ID、加密历史指纹与账户的路由绑定；启用[请求原文调试](DEVELOPMENT-CHECKS.md#请求原文调试)后，客户端请求及响应内容还会写入请求记录，供管理员排查。绑定按客户端密钥、分组、模型和上游配置隔离，沿用 30 天保留期及 `BRIDGE_RESPONSE_BINDING_BYTES` 容量预算。上游存储时间仍由上游决定。查询、删除和续聊必须使用原客户端密钥；上游 WS 会话丢失后不会自动补历史或重放。

HTTP/SSE 模式下，客户端 WS 使用 SSE 事件桥接；WS 优先模式可复用上游 WS 续聊连接。WS 预热 `generate:false` 仅在上游 WS 可用时转发，HTTP 回退不把预热变为计费生成。所有上游连接遵循账户代理配置。响应的 `x-request-id` 为本站请求 ID；上游同名字段通过 `x-upstream-request-id` 保留。

桌面内置生图也可使用同一 Base URL 与 Key：`/v1/images/generations` 和 `/v1/images/edits`，详见 [图片接口](IMAGES.md)。

客户端使用 `/v1/responses` HTTP/SSE 或 `/v1/native` WebSocket，均使用 `Authorization: Bearer <API Key>`。标准 SDK 接入见 [Responses HTTP](RESPONSES.md)。完整消息协议、动态工具回调和运行示例见 [原生会话](NATIVE-DIRECT.md)。`GET /v1/models` 可查询 Key 可见的模型提示列表。

## 设置

使用本机配置账户时，先在“系统设置 → 环境与诊断 → 原生运行环境”检测安装。自动模式需保持官方 Codex 桌面运行；独立模式可指定支持 app-server 的 Codex 可执行文件，无需桌面，详见 [README 安装说明](../README.md#日常使用)。然后创建本机配置实例，读取配置并启用。实例使用自己的工作目录与进程，自动识别本机 Codex 的提供方和凭证：ChatGPT 账户登录、第三方中转（含局域网与公网地址）以及官方 API Key。认证、令牌刷新和模型连接均由原生 app-server 完成。Bridge 进程须能访问同一 CODEX_HOME、系统凭据存储和所需环境变量。更换本机提供方或登录方式后，点击“刷新原生能力”重新读取路由。新建本机配置实例使用 nativeConfig，存储类型为 native-config。这里的“独立”指进程与会话；多个实例可能使用同一上游账户和额度。实例归组后创建同组密钥。

每枚 Key 可配置允许的模型 ID；留空表示不单独限制，实际支持情况由上游决定。Key 的模型名单不是上游能力探测。“系统设置 → 环境与诊断”中的“桌面连接调试”不参与 `/v1/responses` 或 `/v1/native` 调度，也不计入可用模型与 API 就绪状态。使用中转站无需启动桌面调试连接。

管理页面选择本机、局域网或公网地址后，可以复制对应的原生 WebSocket 会话地址。配置网络入口见 [网络接入](NETWORK-ACCESS.md)。

## 使用密钥配置 Codex

在“客户端接入 → API 密钥”点击某一行的终端图标（悬停提示“使用密钥”），选择接入地址、模型 ID 和客户端操作系统，然后点击“复制配置”。地址列表与“接入指南”一致；外部设备需要使用可达的局域网或公开入口，并开启外部访问。

模型默认使用 `gpt-6.1-sol`，`review_model` 跟随所选模型。若密钥有模型限制，候选仅来自允许列表；不包含默认模型时，选择允许范围内的路由模型，再取首个候选。未限制模型时，候选包含默认模型和同分组已启用账户的路由模型，也允许输入其他模型 ID，实际支持情况由上游决定。

生成的完整配置使用 `OpenAI` 自定义提供方和 Responses 协议，通过 `experimental_bearer_token` 直接包含当前密钥；同时包含 `model_reasoning_effort = "max"`、`service_tier = "default"`、`capabilities.remote_compaction = "v2"`、`X-OpenAI-Actor-Authorization = "proxy-managed"`，以及 `features.image_generation = true` 和 `features.goals = true`。`proxy-managed` 是客户端的服务端托管认证标记，实际网关鉴权仍使用当前密钥。显式远程压缩复用原生账户认证，通过 HTTP 收发真实压缩条目，正文仅经过内存。旧导出配置若关闭了远程压缩，可重新导入；中转站内部原生自动压缩后仍可沿用同一线程。

默认使用 HTTP/SSE；勾选“启用 WebSocket”后，Codex 使用同一 Base URL 下的 `/v1/responses` WebSocket（不是 `/v1/native`）。预览框可滚动查看完整内容，复制不会截断。

将配置合并到**客户端用户级**配置文件，保留已有其他配置，然后完全退出并重启 Codex：Windows 为 `%USERPROFILE%\.codex\config.toml`，macOS/Linux 为 `~/.codex/config.toml`；设置了 `CODEX_HOME` 时使用该目录中的 `config.toml`。无需改写 `auth.json`。配置包含完整密钥，不应分享或提交到仓库。客户端与中转站同机时，服务端应使用指向真实上游的独立账户配置，避免请求回到中转站自身。

暂停、过期或缺少明文的密钥无法生成配置；弹窗打开期间检测到密钥失效或删除会自动关闭。自动复制失败或超时时，可在配置框中手动复制。本功能仅生成和复制配置，不写入本机文件、不保存到浏览器存储、不发送模型请求。

## 全部导出与批量导入

在“客户端接入 → API 密钥”点击“全部导出”，下载当前所有未撤销密钥的 JSON 备份，包括已暂停和已过期的密钥，不受搜索、状态筛选或分页影响。文件含完整密钥，请妥善保管。备份保留名称、原密钥、分组 ID、模型权限、启停状态、创建时间、有效期、调用方并发和强制快速模式；不包含历史统计、续聊绑定或上游账户凭据。

点击“批量导入”选择备份文件或粘贴 JSON，每批支持 1–1000 个密钥。默认保留原分组；迁移到另一服务时，可统一指定一个已启用的目标分组。原分组不存在时须选择目标分组，系统不会自动扩大密钥访问范围。暂停和过期状态会保留；新导入记录分配新的内部 ID，原密钥内容不变。

以完整密钥去重，已存在的密钥（含已撤销记录）和文件内重复项自动跳过，不覆盖已有配置。整个批次通过校验后才提交；格式、分组或容量校验失败时整批回滚。连接中断后可重新导入以核对结果。记录总数上限为 1000。

管理接口：`GET /api/api-keys/export` 返回 `{ "version": 1, "keys": [...] }`；`POST /api/api-keys/import` 接收 `{ "backup": <备份对象>, "groupId": <可选目标分组 UUID> }`，返回 `imported`、`skipped` 数量。单项包含 `name`、`secret`、`groupId`、`models`、`state`（`active` / `paused`）、`createdAt`（毫秒）、`expiresAt`（毫秒或 null）、`maxConcurrency` 和必填的 `fastMode`（布尔值）。缺少 `fastMode` 时拒绝导入，不为旧开发备份补默认值。接口使用管理认证，不接受客户端密钥；响应禁止缓存。

## 权限和生命周期

- Key 默认长期有效，支持暂停、恢复和删除。列表中的编辑按钮打开弹窗，可一次修改名称、所属分组、并发和强制快速模式；保存保留原密钥内容、模型范围和到期时间。删除后密钥立即失效，历史请求统计保留。
- 模型授权、分组状态、实例路由与 Key 状态在准入时复核，会话期间每 10 秒复核。
- 同一 Key 改分组后，新请求按新分组准入和结算；原账户与模型仍获授权时，存活的原生线程、工具回调和 Responses WebSocket 连接历史可以继续使用。已有在途请求按原准入分组记账，改分组后的 `response.steer` 不能向该旧轮次追加输入。持久化的响应绑定及独立搜索会话仍按各自的分组作用域校验，详见 [保存与生命周期](RESPONSES.md#保存容量与生命周期)。
- 密钥在本机 SQLite 保存明文以便管理界面复制，鉴权使用摘要。不要共享 data 目录或运行数据库。
- 只接收单一 Authorization Bearer 头，不接受 URL 参数或 Cookie 凭据。
- 管理令牌与调用方 Key 分离，客户端网络入口不挂载管理接口或文件服务。
- Host/Origin 按网络配置验证，反向代理应保留受信任的 Host 并支持 WebSocket Upgrade。

## 并发和统计

Key 并发、服务总容量和账户并发同时生效；普通 Responses 的多个独立 thread 可共享兼容的 app-server 进程，进程数量不等于请求并发。Responses HTTP/SSE 与 `/v1/responses` WebSocket 按请求准入、结算和释放额度，等待客户端工具结果期间不占请求额度。`/v1/native` 使用独占执行器，租约覆盖整个连接，包括工具等待与轮次间空闲，最长 30 分钟。达到容量进入有界等待队列；等待任务被取消或丢弃时释放等待资源。

Responses 按每次请求记录用量，`/v1/native` 的累计用量在会话结束时结算。异常断开可能没有完整用量，缺失值不计为零。鉴权探测不发送模型请求。管理员可通过停止实例结束会话。原生账户可按逻辑线程或会话保持账户亲和，默认 24 小时，可在系统设置中调整；线程复用仍校验 Key、账户、完整历史和当前分组授权；同一 Key 改分组后，原账户和模型仍获授权时可以续用存活线程及工具回调，详见 [Responses 接入](RESPONSES.md#原生传输与兼容范围)。

## 管理接口

| 接口 | 用途 |
| --- | --- |
| GET /api/api-keys | 列出 Key、Base URL 和网络入口 |
| POST /api/api-keys | 创建 Key：name、groupId；可选 models、maxConcurrency、expiresInDays、fastMode |
| POST /api/api-keys/{id} | revision；可选 name、groupId、maxConcurrency、state、fastMode，编辑字段原子保存 |
| DELETE /api/api-keys/{id} | 删除 Key |
| POST /api/api-keys/{id}/group | 调整分组 |
| POST /api/api-keys/{id}/concurrency | 调整 Key 并发 |
| POST /v1/responses | 标准 Responses JSON / SSE |
| GET /v1/responses | Responses WebSocket Upgrade，按请求占额 |
| GET /v1/responses/{id} | API Key 直连按绑定访问上游响应；原生账户不提供成功响应正文存储 |
| DELETE /v1/responses/{id} | API Key 直连删除上游响应；原生账户不提供成功响应正文存储 |
| GET /v1/models | 已鉴权模型目录 |
| GET /v1/native | 已鉴权 WebSocket Upgrade |

## 示例

```powershell
$env:BRIDGE_NATIVE_URL = "ws://127.0.0.1:4317/v1/native"
$env:BRIDGE_API_KEY = "<你的调用方密钥>"
$env:BRIDGE_MODEL = "<已启用模型>"
node examples/native_client.mjs
```

示例调用客户端本地时间函数，会产生真实模型请求。模型连接、认证与原生工具编排由 app-server 处理，中转站只在接入侧管理会话。
