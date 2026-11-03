# 配置手册：从零到手机上能收消息

这份文档按「一台机器 → 两个容器 → 一个公网入口 → 一个 APK」的顺序走一遍。每一步都给了验证命令，做完就知道通没通。

组件关系：

```
手机 App ──HTTPS──> imbo-server ──HTTP──> ntfy ──WebSocket──> 手机 ntfy 客户端 ──> 系统通知
                       │
                       └── PostgreSQL（内嵌在容器 /data）
```

---

## 1. 准备

| 需要 | 说明 |
| --- | --- |
| 一台常驻机器 | VPS / NAS / 树莓派 / 家里的电脑都行；关机就收不到消息 |
| Docker + compose 插件 | Debian/Ubuntu：`curl -fsSL https://get.docker.com \| sh` |
| SMTP 账号 | 登录验证码用；没有就先开 `IMBO_MAIL_LOG_ONLY=1` 把验证码打进日志 |
| 公网入口 | 三选一：Tailscale、Cloudflare Tunnel、自己的域名 + 反代 |

检查：

```bash
docker --version && docker compose version
```

---

## 2. 起服务端和 ntfy

```bash
git clone https://github.com/Iron-Wph/imbo.git
cd imbo/deploy/docker
cp .env.example .env
vi .env            # 见下面变量表
./setup.sh
```

`setup.sh` 做的事：检查 docker/curl → 生成 `.env`（若缺）→ 校验「SMTP 或 log-only 至少一个」→ `docker compose up -d` → 轮询两个 `/v1/health` → 打印地址和下一步。失败会直接把容器日志打出来。

### `.env` 变量表

| 变量 | 必填 | 说明 |
| --- | --- | --- |
| `IMBO_IMAGE` | 是 | `imbo-server:ntfy`（自己构建）或 `ghcr.io/bearisbug/imbo-server:latest`（官方镜像，但官方镜像不含 ntfy 通道） |
| `IMBO_PORT` / `NTFY_PORT` | 否 | 宿主机端口，默认 8080 / 8081，只监听 `127.0.0.1` |
| `IMBO_SMTP_URL` | 二选一 | 例：`smtps://user:pass@smtp.example.com:465` |
| `IMBO_MAIL_LOG_ONLY` | 二选一 | `1` = 不发邮件，验证码打进 `docker compose logs imbo` |
| `IMBO_SIGNUP_EMAIL_DOMAINS` | 否 | 逗号分隔的邮箱域白名单，留空不限 |
| `IMBO_NTFY_TOKEN` | 否 | ntfy 开了访问控制才需要 |
| `IMBO_DOMAIN` / `IMBO_ACME_EMAIL` | 否 | 只用「域名 + 直连 443」时才填，让服务端自己申证书 |
| `IMBO_REVIEW_EMAIL` / `IMBO_REVIEW_CODE` | 否 | 指定邮箱用固定验证码，方便自己测试 |

### 验证

```bash
curl -s http://127.0.0.1:8080/v1/health
# {"status":"ok",...,"push":{"expo":true,"fcm":false,"ntfy":true}}
curl -s http://127.0.0.1:8081/v1/health
# {"healthy":true,"role":"active"}
```

`push.ntfy=true` 才算推送配好了；`false` 说明 `IMBO_NTFY_URL` 没进容器。

---

## 3. 给手机一个能访问的地址

App 的服务器地址必须是 **https**（release 包拒绝明文 http）。三种做法：

### 3.1 Tailscale（私有，最省事）

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up                 # 浏览器里登录，个人版免费
sudo tailscale serve --bg http://127.0.0.1:8080
sudo tailscale serve --bg --https=8443 http://127.0.0.1:8081
tailscale serve status
```

得到：

```
https://<机器名>.<tailnet>.ts.net        → imbo
https://<机器名>.<tailnet>.ts.net:8443   → ntfy
```

首次用 `serve` 时 Tailscale 会要求管理员点一次授权链接（`login.tailscale.com/f/serve?...`）。

手机上装 Tailscale、登录同一账号、打开开关即可访问。**Tailscale 和 Clash 互斥**（都占 Android 的 VPN 通道）：开了 Tailscale 就要关 Clash，反之亦然。

### 3.2 Cloudflare Tunnel（不用装 VPN，手机可以常开 Clash）

```bash
# 临时地址（重启就变）
cloudflared tunnel --url http://127.0.0.1:8080
cloudflared tunnel --url http://127.0.0.1:8081
```

长期用就登录 Cloudflare、建 named tunnel 绑自己的域名。

### 3.3 域名 + nginx

```nginx
server {
  listen 443 ssl;
  server_name imbo.example.com;
  ssl_certificate     /etc/letsencrypt/live/imbo.example.com/fullchain.pem;
  ssl_certificate_key /etc/letsencrypt/live/imbo.example.com/privkey.pem;

  location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;   # /v1/events 的 WebSocket
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 3600s;
  }
}
```

---

## 4. 手机端配置

1. 装 App（debug 包或自己出的 release 包）。
2. 登录页底部点「服务器」，填第 3 步的 https 地址；没有默认值时这里显示「未配置（点此填写）」。
3. 用邮箱收验证码登录。没有 SMTP 时去服务端日志里找 `code=123456`。
4. 账号页「推送」一节会显示本机话题。点「复制」得到订阅链接；点「设置」可以改 ntfy 地址（默认取构建时的 `-Pimbo.ntfyUrl`）。
5. 在这台手机上装 ntfy 客户端，订阅那个链接（形如 `https://ntfy.example.com/imbo-xxxxxxxxxxxx`）。
6. 账号页「检查连接」点一下，应显示「连接正常 · 服务端 0.2.17」。

话题由 `ANDROID_ID` + 包名派生，**重装不变**，ntfy 客户端订阅一次长期有效；换手机或恢复出厂才会变。

---

## 5. 构建自己的 APK（含签名）

### 5.1 生成 keystore（只做一次，务必备份）

```bash
keytool -genkeypair -v -keystore imbo-release.jks -alias imbo \
  -keyalg RSA -keysize 2048 -validity 10000 \
  -dname "CN=Imbo Android, OU=Mobile, O=Imbo, L=City, ST=State, C=CN"
```

`android/keystore.properties`（不要入库，`.gitignore` 已排除）：

```properties
storeFile=/绝对路径/imbo-release.jks
storePassword=***
keyAlias=imbo
keyPassword=***
```

### 5.2 构建

`-Pimbo.baseUrl` / `-Pimbo.ntfyUrl` 都是可选的；不传就得到「服务器未配置」的包，用户自己在登录页填。

```bash
cd android
./gradlew :app:assembleDebug -Pimbo.useMock=false -Pimbo.baseUrl=https://imbo.example.com

# release：一条命令出 APK + AAB，算 sha256，拷到 dist/
./scripts/release.ps1 -BaseUrl https://imbo.example.com -NtfyUrl https://ntfy.example.com   # Windows
./gradlew :app:assembleRelease -Pimbo.baseUrl=https://imbo.example.com -Pimbo.ntfyUrl=https://ntfy.example.com  # 任意平台
```

同一个 APK 可以服务多个部署：地址是运行时设置，不需要重新编译。

---

## 6. CI 出包（可选）

仓库自带的 `.github/workflows/android-release.yml` 在打 `android-v*` tag 或手动触发时构建签名包。

### 仓库 Secrets

| Secret | 内容 |
| --- | --- |
| `IMBO_KEYSTORE_BASE64` | `base64 -w0 imbo-release.jks` |
| `IMBO_KEYSTORE_PASSWORD` | store 口令 |
| `IMBO_KEY_PASSWORD` | key 口令 |

### 仓库 Variables

| Variable | 内容 |
| --- | --- |
| `IMBO_BASE_URL` | 发布包内置的服务端地址 |
| `IMBO_NTFY_URL` | 可选，ntfy 地址；不配则账号页只显示话题、不给订阅链接 |

配置命令（需要 `repo` + `workflow` 权限的 token）：

```bash
gh secret set IMBO_KEYSTORE_BASE64 --repo <owner>/<repo> --body "$(base64 -w0 imbo-release.jks)"
gh secret set IMBO_KEYSTORE_PASSWORD --repo <owner>/<repo> --body '<store 口令>'
gh secret set IMBO_KEY_PASSWORD --repo <owner>/<repo> --body '<key 口令>'
gh variable set IMBO_BASE_URL --repo <owner>/<repo> --body 'https://imbo.example.com'
gh variable set IMBO_NTFY_URL --repo <owner>/<repo> --body 'https://ntfy.example.com'

git tag android-v0.1.0 && git push origin android-v0.1.0
gh run watch --repo <owner>/<repo>
```

产物在 Actions 的 `imbo-android-release` artifact 里：APK、AAB、`checksums.txt`。

> 如果不希望签名密钥存在 GitHub 上：不要配这三个 secret，改为本地用 `keystore.properties` 出包，CI 只跑测试。

---

## 7. 部署自检

```powershell
# Windows
./scripts/check-deployment.ps1 -BaseUrl https://imbo.example.com -NtfyUrl https://ntfy.example.com -Topic imbo-xxxxxxxxxxxx
```

检查三件事：服务端 `/v1/health` 为 ok、`push.ntfy` 为 true、ntfy 健康，最后往话题发一条测试通知（手机上应弹出「imbo 部署自检」）。

不需要发测试通知就加 `-SkipTestNotification`。

---

## 8. 故障排查

| 现象 | 排查 |
| --- | --- |
| 容器反复重启，日志 `set IMBO_SMTP_URL ... or IMBO_MAIL_LOG_ONLY=1` | `.env` 两个都没配；先设 `IMBO_MAIL_LOG_ONLY=1` |
| App 登录「连不上服务器」 | 服务器地址是否 https；手机能否打开 `https://地址/v1/health`；Tailscale/Clash 是否互相顶掉 |
| 能登录但收不到通知 | `/v1/health` 的 `push.ntfy` 是否为 true；ntfy 客户端是否订阅了**本机当前**话题；服务端日志是否有 `push: ntfy unreachable` |
| 通知内容是一串 JSON | ntfy 版本对「根路径 JSON 发布」的解析不同；确认服务端发到 `{base}` 且带 `topic` 字段 |
| ntfy 客户端显示「连接错误」 | 订阅地址是否还能访问（换过端口/域名/隧道）；Tailscale 是否在线 |
| 点通知打不开 App | 手机上点一次允许「ntfy 想要打开 Imbo」；或 `adb shell am start -a android.intent.action.VIEW -d imbo://chat/1` 验证深链 |
| WebSocket 频繁断 | 反代要放行 `Upgrade`/`Connection` 头，并把读超时调大 |

---

## 9. 安全清单

- **keystore 不要入库**：`android/keystore.properties`、`*.jks` 已在 `.gitignore`；离线备份两份，丢了就无法用同包名更新
- **ntfy 话题就是密码**：知道话题的人都能读到通知。公开部署请给 ntfy 开访问控制（`ntfy user add` / `ntfy access`），并把令牌填进 `IMBO_NTFY_TOKEN`
- **限制注册**：`IMBO_SIGNUP_EMAIL_DOMAINS` 只放行自己的域名
- **暴露面**：compose 只监听 `127.0.0.1`，公网入口交给 Tailscale/Tunnel/反代，不要直接把 8080 开到公网
- **SMTP 口令**只放在 `.env`（已 gitignore），不要写进文档或提交
