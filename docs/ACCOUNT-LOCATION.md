# 账号时区与搜索位置

在账户详情的“接入与调度”中，代理设置下方可配置“时区与搜索位置”。新旧账号默认关闭；关闭时不检测出口、不改变现有原生请求。

开启后默认跟随账号代理，或选择自定义。自定义时区必填，使用 IANA 名称，例如 `America/New_York`、`Asia/Shanghai`；国家、省／州和城市选填，填写省／州或城市时须选择国家。时区不会自动推断城市。关闭功能保留自定义内容。

勾选启用并选择“跟随账号代理”后，可以先点击“检测时区”。检测仅生成预览，不保存配置、不更改当前有效位置、不重启实例；确认预览后点击“保存检测结果”才应用。可放弃结果，继续沿用现有设置。预览保留 5 分钟，绑定账号、代理和配置版本，过期或配置变化后需重新检测。自定义或关闭功能仍使用“保存时区与位置”。

保存后立即重启该账号运行中的实例，中断该账号的当前推理、长连接和登录；停止的账号保持停止。配置切换失败时暂不接收新任务，可点击重试。该行为与账号代理设置一致。

## 跟随账号代理

优先通过账号当前连接访问 `https://chatgpt.com/cdn-cgi/trace`，读取该站点观察到的出口 IP；再请求 `https://ipinfo.io/{IP}/json` 查询这个指定 IP 的国家、地区、城市和 IANA 时区。IPinfo 失败、限流或返回无效结果时，回退到 `https://ipwho.is/{IP}`，仍查询同一个已捕获的出口 IP，不重新使用定位服务自身看到的出口。

ChatGPT 诊断端点无法访问或返回无效 IP 时，才直接使用 `https://ipwho.is/` 检测当前连接的位置。此时备用结果可能受分流影响，用户应确认预览后保存。探测最多 2 秒，每个定位服务最多 3 秒，包含代理解析的总预算 8 秒；响应上限 64 KiB。不会向诊断端点发送账号 Token、Cookie、Authorization 或模型请求。查询定位服务时仅传递出口 IP，不传递账号凭据。

选择 ChatGPT 域名是为了更接近模型服务的代理分流规则；封锁状态会变化，不能保证所有网络都无法直连。直连模式检测直连出口；自动模式优先遵循启动环境的代理变量，未指定时读取 Windows 当前用户或 macOS 系统手动 HTTP/HTTPS 代理。检测和新启动的账户实例使用相同解析入口；系统设置变更后已运行实例需重启，检测则立即读取新设置，预览保存仍校验代理指纹。不会根据国家名称过滤或伪造检测结果。结果不符时可放弃预览或改用自定义时区。

参考：[ChatGPT 诊断端点](https://chatgpt.com/cdn-cgi/trace)、[大陆可达性测量](https://en.greatfire.org/https/chatgpt.com)、[IPWhois 指定 IP 查询文档](https://ipwhois.io/documentation)。

首次启用、代理变更和缓存过期会触发自动检测；手动“检测时区／重新检测”独立生成预览。成功结果缓存 24 小时，到期由后续请求触发后台刷新；自动检测失败至少等待 5 分钟再重试。同一出口的自动并发检测合并；手动预览重新探测，不改变有效缓存。

升级到 ChatGPT 出口优先检测后，不再加载旧版定位服务直接检测生成的缓存，后续请求重新检测。

检测失败时，若同一账号、同一代理配置已有成功缓存，则继续使用并显示提示；否则保留原请求行为，不阻断推理。更换代理后不会沿用旧出口缓存。检测缓存单独保存在运行目录的 `operations/location-cache.sqlite3`，后台刷新不修改配置 revision。

保存预览且有效位置变化时立即重启账号；仅检测不重启。后台刷新不打断当前请求，后续请求通过位置指纹淘汰旧会话。原生长连接中的位置保持会话创建时的值，每个新轮次按该时区更新日期。

## 请求效果

有效位置存在时，使用官方 `include_environment_context_time=false` 关闭原生环境日期、时区注入，由中转站每轮注入指定时区的当地日期。只改写完整且角色匹配的环境／桌面时间上下文，不替换普通消息中的日期、地点，不修改 Unix 时间戳。

搜索配置同时使用该位置；不会因设置位置而启用搜索。客户端搜索工具的声明在参数归一化后应用位置，因此不会被兼容层清除。HTTP、原生会话和压缩共用 RPC 处理入口；原生 WebSocket 的搜索能力范围不变。

该设置不改变系统时区、本机 Codex 全局配置、出口 IP 或账号所属地区。

## 管理接口

- `POST /api/accounts/:id/location`：`{ revision, config: { enabled, source: "proxy" | "custom", custom: { timezone, country, region, city } }, previewId?, retry? }`；启用跟随代理时必须提供 `previewId` 确认检测结果，服务端只接受该账号未过期且代理未变更的预览。`retry: true` 只用于重新应用已保存且切换失败的同一设置。返回更新后的 operations 概览。
- `POST /api/accounts/:id/location/detect`：`{ revision }`；允许在启用保存前预览当前账号连接，返回 `{ revision, preview: { id, location, detectedAt } }`，不会更改配置或有效位置。
- `GET /api/location/options`：返回后端支持的 IANA 时区及 ISO 3166-1 国家代码。
- 账号概览增加 `location`、`locationStatus`，后者包含状态、有效位置、检测时间和错误提示；实例重启状态沿用 `proxyStatus`。

## 验证

Rust 命令统一通过内存检查入口运行：

```powershell
node scripts/cargo-with-memory.mjs test --lib
$env:BRIDGE_NATIVE_INSTALLATION = (Resolve-Path '.cache/launch-config.json').Path
node scripts/cargo-with-memory.mjs test --test native_location -- --ignored
pnpm --filter @codex-relay/console test account-location.spec.ts account-proxy.spec.ts
```

原生验证使用隔离目录和本地模拟模型，不使用真实账号凭据或模型额度。
