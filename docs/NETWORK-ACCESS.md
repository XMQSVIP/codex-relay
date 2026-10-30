# 局域网与公网接入

默认访问 `http://127.0.0.1:4317`，首次启动后台免登录，外部访问开启；已有访问设置保持不变。在“系统设置 → 访问安全”中可独立选择免登录 / 密码登录，以及是否允许公网用户访问；开启公网不强制设置密码。免登录且开启公网时，能连接服务的外部用户可直接管理后台。客户端 API 始终使用独立 API Key。

公网开关同时控制后台页面、管理 API、主端口 `/v1` 和所有独立 HTTP / HTTPS 客户端入口，保存后立即生效。关闭时只接受本机回环连接与回环 Host；不接受外部来源伪造回环 Host，也不信任 `X-Forwarded-Host` / `Forwarded`。反向代理须保留实际 Host。主端口监听所有 IPv4 网卡，由请求边界执行访问限制；开关不会修改防火墙、路由器或 DNS。开启后可通过 `http://<服务电脑的 IP>:4317/` 访问后台，客户端使用同地址的 `/v1`。

后台密码以加盐 PBKDF2-HMAC-SHA256（600,000 次）保存于 `data/accounts/admin-access.json`；明文密码不落盘。登录会话为 12 小时 HttpOnly / SameSite=Strict Cookie，不写入浏览器存储；退出或重启服务后需重新登录；修改访问设置会注销其他后台会话，保存者会取得新会话。修改已有密码或公网设置时验证当前密码。登录最多每分钟 5 次尝试。

首次开启密码登录需提供非空密码；修改时新密码留空保留原密码。新密码只输入一次且明文可见，当前密码与登录密码仍遮蔽显示；前后端不再设置 8–128 字符或单密码 512 字节限制。请求体上限仍生效：登录 JSON 最多 4 KiB，访问设置 JSON 最多 64 KiB，因此密码长度仍需使完整登录请求落在 4 KiB 内。

客户端还可配置多个独立 HTTP / HTTPS 监听，共用密钥库、统计、并发上限和等待队列。独立端口只提供客户端 API，不提供后台页面或管理 API。模型连接由原生 app-server 直接建立。可选 CLI `--require-admin-token` / 启动脚本 `-RequireAdminToken` 额外要求每次启动生成的管理令牌，与界面密码设置叠加。

## 修改服务端口

Windows、macOS 和 Linux 共用项目（或便携包）根目录的 `relay.config.json`：

```json
{
  "port": 4317
}
```

将 `port` 改为所需端口（1024–65535），先在管理页面停止中转站，再重新启动。比如改成 `8080` 后，管理页面为 `http://127.0.0.1:8080/`，客户端 Base URL 为 `http://127.0.0.1:8080/v1`；客户端地址也要同步修改。

双击启动器、`scripts/Start-Relay.ps1`、`scripts/Start-Relay.sh` 和 `pnpm start` 都使用此配置。直接运行二进制时，请先进入项目或便携包根目录：配置文件按当前工作目录查找。命令行 `serve --port 8080`（PowerShell 启动脚本使用 `-Port 8080`）优先于文件配置，不修改文件。缺少配置文件时默认使用 4317；文件内容无效时启动失败并显示错误。更新旧版时需要重新构建或更换程序和启动器，只有修改配置文件无法让旧程序支持它。

此文件控制后台与主 API 共用的端口；独立 API 监听仍在 `data/network.json` 中配置。修改后需要重启，且新端口不能与独立监听重复。

## HTTP：局域网或公网

复制 `examples/network.http.json` 为 `data/network.json`，替换为实际地址，例如：

```json
{
  "listeners": [{
    "listen": "0.0.0.0:4318",
    "baseUrls": [
      "http://192.168.1.10:4318/v1",
      "http://bridge.example.com:4318/v1"
    ],
    "allowedOrigins": []
  }]
}
```

```powershell
.\scripts\Start-Relay.ps1 -NetworkConfig data/network.json
# 或直接使用编译后的程序
.\target\debug\codex-relay.exe serve --network-config data/network.json
```

`listen` 是本机绑定地址，`0.0.0.0` 表示所有 IPv4 接口，也可填写特定网卡 IP；IPv6 用 `[::]:4318` 或特定 IPv6 地址。`baseUrls` 是实际分发给客户端的完整地址，必须以 `/v1` 结尾；它同时生成该监听的 Host 白名单。添加多个网卡 IP、公网 IP 或域名时分别列出，客户端应选择自己可达的地址。公网映射的外部端口与监听端口可以不同。

局域网客户端使用主机的局域网 IP。公网部署还需要实际公网入口、DNS（使用域名时）、路由器端口映射或已有隧道，把客户端 TCP 端口转发到本机对应监听。只修改 `baseUrls` 不会创建 DNS 或端口映射。Windows 防火墙如开启，应仅允许所需 API 端口和来源；程序不会更改防火墙或路由器。需要远程后台时转发主端口；仅开放客户端时转发独立客户端端口。

HTTP 不强制跳转 HTTPS，适用于局域网和公网；连接不加密，密钥和正文以明文传输。HTTP 与 HTTPS 可以同时启用。

## 原生 HTTPS

使用 `examples/network.http-https.json`，其中 HTTPS 监听示例：

```json
{
  "listen": "0.0.0.0:4319",
  "baseUrls": ["https://bridge.example.com:4319/v1"],
  "allowedOrigins": [],
  "tls": {
    "certificate": "certs/fullchain.pem",
    "privateKey": "certs/private-key.pem"
  }
}
```

把它放入 `listeners` 数组。证书链与私钥使用 PEM，路径相对于网络配置文件所在目录；例如配置位于 `data/network.json`，则文件位于 `data/certs/`。证书须匹配客户端使用的域名或 IP，并受客户端信任。自签测试证书需要在测试客户端单独建立信任。

桥负责 TLS 握手，不自动申请或续期证书；证书更新后等待请求完成并重启服务。任一配置无效、端口无法绑定或证书加载失败，整个服务启动失败，不会只开放部分入口。HTTP / HTTPS 共享 Key，撤销或并发调整立即对所有入口生效。

## 由现有反向代理提供 HTTPS

使用 `examples/network.reverse-proxy.json`，桥仅监听 `127.0.0.1:4318`，对客户端声明 `https://bridge.example.com/v1`。HTTPS 反代模式要求内部 HTTP 监听为回环地址。

以下是与桥同机运行的 Caddy 配置示例：

```caddyfile
bridge.example.com {
    reverse_proxy 127.0.0.1:4318
}
```

Caddy 默认向此 HTTP 上游保留请求 Host，并支持 WebSocket Upgrade；无需设置负值 `flush_interval`（该模式会改变客户端提前断开时的取消行为）。保留默认不重试策略，避免重复提交模型请求。见 [Caddy reverse_proxy 文档](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy)。

域名指向入口、80 / 443 可达等条件满足后，Caddy 可自动申请和续期公网证书，具体要求见 [Automatic HTTPS](https://caddyserver.com/docs/automatic-https)。本项目没有安装或启动 Caddy。若使用其他代理，同样应保留外部 Host、完整 `/v1` 路径、Authorization 和 WebSocket Upgrade，允许足够的排队与执行时间，并把下游断开传播到桥；只开放客户端时指向独立端口；同时开放后台时可将完整站点反代到主端口 4317，并在设置中开启公网访问。

桥不会使用 `X-Forwarded-Host` 或 `Forwarded` 绕过 Host 白名单，也不据此推断客户端身份。独立监听的反向代理对外地址需要写入其 `baseUrls`；主端口按实际 Host 校验同源 Origin。

## 浏览器来源

桌面应用、CLI 和服务端客户端通常不发送 Origin，`allowedOrigins: []` 即可。若通过支持 Authorization 请求头的适配客户端携带网页 Origin，将网页的确切来源填写为 `https://client.example.com`，包含必要端口，不包含业务路径；不支持 `*`、`null` 或凭据 Cookie。预检只开放已实现的路径、方法和 Authorization / Content-Type / X-Request-ID 请求头，实际请求仍需 API Key。HTTPS 网页通常不能直连 HTTP API，应使用 HTTPS 入口。

## 分发与验证

在本机工作台“客户端接入 → 网络接入”选择目标地址，创建绑定模型与分组的 Key，复制配置给调用方：

```json
{
  "base_url": "http://192.168.1.10:4318/v1",
  "api_key": "<创建时显示的 API Key>",
  "model": "<已启用路由中的确切模型 ID>"
}
```

支持 Responses 的客户端可填写 HTTP Base URL 和 API Key；也可使用原生 WebSocket 会话，详见 [调用方接入](CLIENT-ACCESS.md) 和 [Responses 支持范围](RESPONSES.md)。只支持 Chat Completions 的客户端不能直接调用本项目。

在另一台电脑的 PowerShell 中设置 `$env:BRIDGE_API_KEY` 后验证：

```powershell
$bridgeBase = 'http://192.168.1.10:4318/v1'
$bridgeHeaders = @{ Authorization = "Bearer $env:BRIDGE_API_KEY" }
Invoke-RestMethod "$bridgeBase/models" -Headers $bridgeHeaders
```

返回 200 和模型列表说明地址与 Key 可用；空列表表示 Key 的模型 / 分组与启用路由没有交集。未带 Key 返回 401，未配置的 Host / Origin 返回 403，客户端端口访问 `/api/overview` 返回 404。模型列表检查不会调用上游模型。

网络配置最多 8 个监听，每监听 16 个 Base URL、32 个来源，文件上限 64 KiB；不允许使用管理端口。修改配置后重启生效；没有独立网络配置时，主端口也可在开启公网访问后供外部连接。`data/` 与证书文件已被 Git 忽略。

`node scripts/cargo-with-memory.mjs test --locked --test network --test admin_access` 使用临时 Key、密码和证书验证 HTTP / HTTPS 实际握手、证书校验、Host / Origin / HTTP/2 authority、CORS、管理隔离、密码会话、并发队列与关闭。公网可达性需要在实际公网入口配置完成后从外部网络验证。

浏览器原生 WebSocket API 无法设置 Authorization 请求头，需由自己的受信任后端适配；不要把 Key 放入 URL。客户端 HTTPS 是接入侧 TLS，模型侧 TLS 始终由 app-server 处理。
