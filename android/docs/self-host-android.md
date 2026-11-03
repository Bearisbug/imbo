# 自建服务端 + 跨网络访问（安卓端）

官方 App 连的是 `imbo.dev` 的服务器；自建的话，服务端在哪、用什么地址，都得自己定。这份文档写清楚三件事：服务端起在哪、怎么让手机在外网也能连上、App 里怎么填。

## 1. 服务端

已有镜像，单二进制，自带 PostgreSQL：

```sh
docker run -d --name imbo-local -p 8080:8080 -v imbo-data:/data \
  -e IMBO_LISTEN=0.0.0.0:8080 \
  -e IMBO_SMTP_URL='smtps://user:pass@smtp.example.com' \
  ghcr.io/bearisbug/imbo-server:latest
```

不配 `IMBO_SMTP_URL` 也能跑，但验证码发不出去——本地开发常用 `IMBO_MAIL_LOG_ONLY=1`，验证码直接打进 `docker logs imbo-local`。

**正式给人用必须配 SMTP**，否则别人收不到登录码。审核/演示账号可以用固定码：

```sh
-e IMBO_REVIEW_EMAIL=you@example.com -e IMBO_REVIEW_CODE=123456
```

## 2. 让手机能从外网连上（三种方式）

### 2.1 Tailscale（推荐：不用买域名、不开端口）

电脑和手机各装一个 Tailscale，登录同一账号，组成私有网络。

电脑上把本机的 8080 暴露成带合法证书的 https：

```sh
# 后台常驻，Tailscale 会给一个 https://<机器名>.<tailnet>.ts.net 地址
tailscale serve --bg http://127.0.0.1:8080

# 看分配到的地址
tailscale serve status
```

拿到的地址形如 `https://my-pc.tail1234.ts.net`，证书由 Tailscale 自动签发（`.ts.net` 是公共后缀，手机系统信任）。

优点：手机用 4G 也能连；没有公网 IP、不用路由器端口映射；服务端只监听本机，外部扫不到。

注意：

- 手机要装 Tailscale 并登录同一个 tailnet，否则解析不到 `.ts.net`。
- `tailscale serve` 默认代理 443，`tailscale serve status` 里能看到实际地址。
- 机器名或 tailnet 改名后地址会变，App 里改一下即可（见第 3 节）。

### 2.2 有 VPS + 域名

直接用仓库自带的 `deploy/` 脚本部署，配 `IMBO_DOMAIN` 和 `IMBO_ACME_EMAIL`，服务端会用 Let's Encrypt 自动签证书，监听 443。App 里填 `https://你的域名`。

### 2.3 临时隧道（cloudflared）

没有域名、只想快速试一次：

```sh
cloudflared tunnel --url http://127.0.0.1:8080
```

会打印一个 `https://xxx.trycloudflare.com` 地址。缺点：每次重启地址都变，适合临时联调。

## 3. App 里填地址

地址不再是编译期写死的，两个入口：

- **登录页底部**：「服务器 · 主机（默认）」，点一下就能改。
- **账号页**：「服务器」一节，右侧「更换」。

规则：

- 不写协议时自动补 `https://`；末尾斜杠会被去掉；支持 `host:port` 和子路径前缀。
- 换服务器会立刻退出登录（token 属于旧服务器），需要在新服务器上重新收验证码。
- **release 版只允许 https**（debug 也允许 http，方便局域网联调）。填 `http://` 会提示「正式版只能连 https 地址」。

## 4. 排错

| 现象 | 检查 |
| --- | --- |
| 登录时提示连不上 | 手机 curl 一下：`curl -s http://<主机>:8080/v1/health`，应返回 `{"status":"ok"}` |
| health 通但收不到新消息 | `/v1/events` 走 WebSocket，确认隧道/反代转发了 `Upgrade` 头；`tailscale serve` 原生支持 |
| 提示证书错误 | 用 Tailscale 的 `.ts.net` 地址或正规域名；自签证书 Android 不认 |
| 填了地址仍走旧服务器 | 账号页确认「服务器」显示的是新主机；换地址后必须重新登录 |
| 验证码收不到 | 服务端是否配了 SMTP；本地开发看 `docker logs imbo-local` 里的 `mail-log-only code=...` |

## 5. 和官方 imbo.dev 的差别

- 官方服务器的地址写死在发布包里；自建版本地址由用户填，所以**同一个 APK 可以连任何服务器**。
- 官方的推送走 Expo 中转；自建 + 原生安卓需要 FCM 通道（见 `docs/push-fcm.md`，服务端打补丁后生效）。
- 订阅/内购只在官方服务器上有，自建不存在计费。
