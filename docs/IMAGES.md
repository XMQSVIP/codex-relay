# 独立图片生成与编辑

原生账户的兼容图片请求优先使用原版 app-server 的内置工具。蒙版、指定尺寸/质量等工具无法表达的请求在发送前选择原有 HTTP 直连。两条路径均保留账户调度、取消和结算。独立实验客户端见下方[本机工具实验](#本机工具实验)。

Codex 桌面的内置 `image_gen` 会在模型调用之外发送独立图片请求。中转站提供 `POST /v1/images/generations`（JSON）与 `POST /v1/images/edits`（JSON 或 multipart/form-data），沿用客户端 Base URL 与 API Key。无需为使用中转站的客户端额外配置 `OPENAI_API_KEY`。

请求须提供 `model`、非空 `prompt`。桌面通常指定 `gpt-image-2`；中转站不会替换模型。图片模型必须属于当前 Key 的模型授权；账户的生图开关、编辑请求的图片输入开关、分组、额度、并发与排队约束继续生效。上游仍需支持该图片接口和模型。

支持 `stream=false` 或省略 stream，返回完整 JSON；尚不支持图片 SSE。请求体上限 32 MiB，结果上限 64 MiB，超时 30 分钟。直连保持 multipart 图片/蒙版及边界原字节，JSON 规范化键后转发，并保留上游 base64/URL、generation_id、背景、用量等字段。官方工具路径另受 8 MiB 原生消息限制，输入不适配时提前选择直连；原生输出超限会报错而不重发。中转站不下载上游返回的 URL。

官方图片工具需要符合原生可用性条件的 ChatGPT 登录态；普通 API Key 登录和免费计划在准备阶段选择直连。工具路径要求 `gpt-image-2`、单张结果、`quality=auto`、`size=auto`、`response_format=b64_json`（除 model 外均可省略），编辑支持最多五张内联 PNG/JPEG/WebP。Key 还须授权账户配置的文本模型，因为工具流程会新增文本推理。官方程序执行生图后由中转站提取 base64；`created` 为中转站收到结果的时间，`usage` 为 null，`native_usage` 单独提供原生会话统计，不能当作图片 token。`transport=official-app-server-tool-turn` 标明结果经过适配，不能保证原始上游的未知字段或错误 HTTP 状态。工具调用由模型选择，提示词不能保证模型严格保持所有参数；没有预期工具结果时返回错误。

不符合上述参数、Key 未授权辅助文本模型或提供方在准备阶段未声明内置能力时，使用原来的直连；API Key 账户仍直连。直连保留 multipart 蒙版、指定尺寸等原始参数及上游字段，具体支持取决于上游。原生账户的图片响应头 `x-codex-relay-upstream-transport` 为 `native-tools` 或 `direct-http`。已发出原生 turn 后的失败、超时、取消绝不回退直连重发。直连复用原生认证及代理，复杂 gateway OAuth/AWS 动态认证仍受原有兼容限制。

请求列表显示“图片生成 / 图片编辑”。已认证请求的参数错误、拒绝、上游失败均有请求 ID 和错误记录；图片 token 未提供时保持未知，不以工具辅助文本模型的 token 代替。直连错误保留上游 HTTP 状态与 JSON；原生错误按官方事件适配。普通统计不保存图片和提示词。无效 API Key 在认证阶段拒绝。下游断开、账号停止、Key 撤销或服务退出会取消请求，原生进程树退出后释放执行目录和额度。官方图片工具可能在其账户 home 的 generated_images 下保留图片文件，与原生程序自身行为一致。

验证入口：

```powershell
node scripts/cargo-with-memory.mjs test --lib images --test images
python scripts/test_images_api.py --relay-binary target/release/codex-relay.exe
python scripts/test_native_tools_relay.py --relay-binary target/debug/codex-relay.exe
```

模拟上游检查图片字节、multipart 文件/蒙版、账号认证头、错误状态与记录、用量及密钥限制/撤销；同时启动未修改的官方桌面 app-server，以合成登录状态执行 `functions.exec → image_gen → 中转站图片接口`，确认图片保存成功。可选真实验收会消费一次图片请求，使用隔离中转站及当前用户的原生提供方配置，不修改运行中的服务：

```powershell
python scripts/test_images_api.py --relay-binary target/release/codex-relay.exe --real-upstream --output .cache/images-real-puppy.png
```

## 本机工具实验

`scripts/native_tools_client.py` 直接连接本机原版 app-server，使用当前原生配置和登录状态，不经过中转站 Key、分组、排队或统计。需要 Python 3.10+ 和已检测的官方安装，无额外 Python 包；以下命令会产生真实模型及工具请求，消耗当前账户额度，`--output` 必须是尚不存在的目录：

```powershell
python scripts/native_tools_client.py --tool image --prompt "画一只小狗，白色背景" --output .cache/official-puppy
python scripts/native_tools_client.py --tool image --prompt "把背景改为蓝色" --image .cache/official-puppy/image-1.png --output .cache/official-puppy-edit
python scripts/native_tools_client.py --tool search --prompt "查找 Rust 官方网站并给出来源" --output .cache/official-search
```

每次调用创建临时会话，结束后停止所属进程树，失败或取消不重放；默认超时 30 分钟，可用 `--timeout` 缩短。编辑最多五张参考图，每张 8 MiB。工具由文本模型选择，不保证调用次数；图片须检查工具状态，不能只看 turn 完成。当前图片 RPC 不返回完整图片 usage，搜索引用依赖同一原生会话，独立 CLI 调用之间不延续引用。工具参数与 HTTP 接口的差异见上文，合成验证入口见 [协议探测与实验](DEVELOPMENT-CHECKS.md#协议探测与实验)。
