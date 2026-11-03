# 用 Tailscale 做公网访问（自用，免费够）

Tailscale 把电脑和手机拉进同一个私有网络（WireGuard），不用公网 IP、不用开端口、不用买域名。个人版免费：3 个用户、100 台设备，个人自用足够。

配合 `tailscale serve`，还能拿到 `https://<机器名>.<tailnet>.ts.net` 这种带合法证书的地址——release 包要求 https，正好满足。

> 没有常驻机器？`imbo/deploy/docker/README.md` 里有 VPS 从零到可用的完整命令（装 Docker、load 镜像、compose 起 imbo + ntfy、再配 Tailscale）。

## 1. 常驻机器上装 Tailscale

Windows：

```powershell
winget install --id Tailscale.Tailscale -e
# 或从 https://tailscale.com/download/windows 下载安装
```

Linux（VPS / 树莓派 / NAS）：

```bash
curl -fsSL https://tailscale.com/install.sh | sh
```

装完登录（会打印一个 URL，浏览器里确认）：

```bash
tailscale up
tailscale status          # 看到自己的机器名和 100.x.y.z
```

## 2. 把 imbo 服务端暴露成 https

前提：imbo 容器已经在跑，本机 `http://127.0.0.1:8080/v1/health` 返回 `ok`。

```bash
tailscale serve --bg http://127.0.0.1:8080
tailscale serve status
# 输出里会有：https://<机器名>.<tailnet>.ts.net → http://127.0.0.1:8080
```

这个地址由 Tailscale 自己续证书，不用配 nginx/caddy。

## 3. ntfy 也要能被手机访问

两种做法，任选：

```bash
# A. 也给 ntfy 一个 https 入口（推荐，手机端不用记 IP）
tailscale serve --bg --https=8443 http://127.0.0.1:8081
# 得到 https://<机器名>.<tailnet>.ts.net:8443

# B. 直接用尾网 IP（ntfy 客户端允许 http，因为流量本身在 WireGuard 里）
#    http://100.x.y.z:8081
```

> App 自己的服务端地址必须是 https（release 包会拒绝明文 http）；ntfy 的链接是给 ntfy 客户端打开的，http 也能用。

## 4. 手机上装 Tailscale

1. 装 Tailscale App，登录**同一个账号**，把开关打开。
2. 浏览器打开第 2 步的 `https://<机器名>.<tailnet>.ts.net/v1/health`，看到 `{"status":"ok",...}` 就通了。
3. 打开 Imbo → 登录页底部「服务器」→ 填这个 https 地址；账号页有「检查连接」，点一下会显示「连接正常 · 服务端 0.2.17」或失败原因。
4. 账号页「推送」里复制订阅链接，在 ntfy 客户端里订阅（地址是第 3 步的 ntfy 入口 + 话题）。

## 5. 出正式包

先自检，再构建，再打 tag：

```powershell
cd imbo/android

# 1) 确认地址真的活着（顺带往手机话题发一条测试通知）
./scripts/check-deployment.ps1 `
  -BaseUrl https://imbo.<机器名>.<tailnet>.ts.net `
  -NtfyUrl https://ntfy.<机器名>.<tailnet>.ts.net:8443 `
  -Topic imbo-xxxxxxxxxxxx

# 2) 用真实地址出包（签名 + 校验和 + 自动拷到 dist/）
./scripts/release.ps1 `
  -BaseUrl https://imbo.<机器名>.<tailnet>.ts.net `
  -NtfyUrl https://ntfy.<机器名>.<tailnet>.ts.net:8443

# 3) 让 CI 出包（仓库里先配好 secrets 和 variables，见 docs/delivery.md）
git tag android-v1.0.0
git push origin android-v1.0.0
```

## 5b. 手机上要常开 Clash 怎么办

Android 同一时刻只能有一个 `VpnService`：Tailscale 和 Clash（VPN/TUN 模式）互斥，做不到「Clash 规则代理 + Tailscale 同时在线」。三种选择：

| 方案 | 手机端 | 代价 |
| --- | --- | --- |
| 公网 https（推荐给别人用） | 只开 Clash，Imbo 走普通 https | 服务端要暴露公网、配证书/隧道 |
| Tailscale 当唯一 VPN，PC 做出口节点 | 只开 Tailscale | PC 必须常开，Clash 在 PC 上跑 |
| 手动切换 | 用 Imbo 开 Tailscale，用别的代理关掉 | 最省事但最麻烦 |

公网 https 的具体做法见 `imbo/deploy/docker/README.md` 的「4b. 不用 Tailscale 的公网方案」（Cloudflare Tunnel 或域名 + nginx）。
## 6. 注意

- **常驻机器关机就断**。要长期可用，选一台一直开的机器（VPS / NAS / 树莓派），别用会睡眠的笔记本。
- Tailscale 的 `serve` 只监听尾网接口，公网扫不到；这比把 8080 直接映射到公网安全得多。
- 想让别人也能用：在 Tailscale 后台把对方邀请进 tailnet（免费额度 3 用户），或者改用 VPS + 域名的方案。
- 手机记得把 Tailscale 排除在电池优化之外，否则后台可能被系统断连。
