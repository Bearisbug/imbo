# 在 VPS 上一键起 imbo-server + ntfy

这套 compose 只监听 `127.0.0.1`，公网入口交给 Tailscale（`tailscale serve`）或你自己的反向代理。好处是不用改安全组、不暴露端口，也不需要给 VPS 配代理。

## 0. 最快路径（已实测）

```bash
git clone https://github.com/Bearisbug/imbo.git && cd imbo/deploy/docker
cp .env.example .env        # 第一次跑保持 IMBO_MAIL_LOG_ONLY=1
./setup.sh                  # 起容器、等健康、打印下一步
./setup.sh --tailscale      # 如果要用 Tailscale 私有访问（需已 tailscale up）
```

`setup.sh` 会检查 Docker / compose 插件、生成 `.env`、校验「SMTP 或 mail-log-only 至少配一个」、起两个容器、轮询 `/v1/health`，最后打印服务端地址、ntfy 地址和出包命令。它不改系统配置、不装东西。

实测记录（本机 Docker Desktop，换端口 18090/18091 跑）：脚本退出码 0，两个健康检查都返回 ok。

## 1. 装 Docker（Debian / Ubuntu）

```bash
curl -fsSL https://get.docker.com | sh
sudo systemctl enable --now docker
sudo usermod -aG docker $USER && newgrp docker
```

> 国内机器拉 Docker Hub 慢的话，先配镜像源，或者把本机镜像 `docker save imbo-server:ntfy | gzip > imbo.tgz` 传上去 `docker load`。

## 2. 拿到镜像

两种方式：

```bash
# A. 官方镜像（如果用的是上游未打 ntfy 补丁的版本，推送会走不到 ntfy）
docker pull ghcr.io/bearisbug/imbo-server:latest

# B. 本机构建、含 ntfy 通道的镜像（推荐，和手机端配套）
#    本机执行：
docker save imbo-server:ntfy | gzip > imbo-server-ntfy.tgz
scp imbo-server-ntfy.tgz user@vps:~/
#    VPS 执行：
gunzip -c imbo-server-ntfy.tgz | docker load
```

## 3. 起服务

```bash
git clone https://github.com/Bearisbug/Quilt.git   # 或用 scp 把 deploy/docker 拷过去
cd Quilt/imbo/deploy/docker

cp .env.example .env
vi .env       # 至少改 IMBO_SMTP_URL（或先保持 IMBO_MAIL_LOG_ONLY=1 跑通）

docker compose up -d
docker compose ps
curl -s http://127.0.0.1:8080/v1/health   # {"status":"ok",...}
curl -s http://127.0.0.1:8081/v1/health   # {"healthy":true,...}
```

## 4. 暴露成 https（Tailscale）

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up

sudo tailscale serve --bg http://127.0.0.1:8080
sudo tailscale serve --bg --https=8443 http://127.0.0.1:8081
sudo tailscale serve status
# https://<机器名>.<tailnet>.ts.net      → imbo
# https://<机器名>.<tailnet>.ts.net:8443 → ntfy
```

手机装 Tailscale 登录同一账号，浏览器打开 `https://<机器名>.<tailnet>.ts.net/v1/health` 能看到 `{"status":"ok"}` 就通了。

## 4b. 不用 Tailscale 的公网方案

如果使用者手机上要常开 Clash（VPN 通道被占），就别让他们装 Tailscale。两种做法：

**A. Cloudflare Tunnel（最快，自带 https，不用域名备案）**

```bash
# 临时隧道：会给一个 https://xxx.trycloudflare.com，重启就变
cloudflared tunnel --url http://127.0.0.1:8080

# ntfy 同理（另开一个终端）
cloudflared tunnel --url http://127.0.0.1:8081
```

长期用就登录 Cloudflare、建 named tunnel、绑自己的域名。缺点是申请 Cloudflare 账号，且流量过第三方。

**B. 自己的域名 + 反向代理**

```nginx
server {
  listen 443 ssl;
  server_name imbo.example.com;
  ssl_certificate     /etc/letsencrypt/live/imbo.example.com/fullchain.pem;
  ssl_certificate_key /etc/letsencrypt/live/imbo.example.com/privkey.pem;

  location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;      # /v1/events 的 WebSocket
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 3600s;
  }
}
```

ntfy 可以放 `ntfy.example.com` 或同域 8443，证书同样处理。

> App 的服务器地址必须是 **https**（release 包拒绝明文 http）；局域网调试用 debug 包才允许 http。
## 5. 让验证码真的发得出去

`.env` 里填 SMTP，例如用 Resend / 阿里云邮件推送 / 自建 Postfix：

```env
IMBO_SMTP_URL=smtps://用户名:口令@smtp.example.com:465
IMBO_MAIL_LOG_ONLY=0
```

改完 `docker compose up -d` 生效。没有 SMTP 时保持 `IMBO_MAIL_LOG_ONLY=1`，验证码会打在 `docker compose logs imbo` 里。

## 6. 出包（在开发机上）

回到开发机：

```powershell
cd android
./scripts/check-deployment.ps1 `
  -BaseUrl https://<机器名>.<tailnet>.ts.net `
  -NtfyUrl https://<机器名>.<tailnet>.ts.net:8443 `
  -Topic imbo-xxxxxxxxxxxx

./scripts/release.ps1 `
  -BaseUrl https://<机器名>.<tailnet>.ts.net `
  -NtfyUrl https://<机器名>.<tailnet>.ts.net:8443
```

## 7. 备份与升级

```bash
# 备份数据库卷
docker run --rm -v imbo-data:/data -v $PWD:/backup alpine \
  tar czf /backup/imbo-$(date +%F).tgz -C /data .

# 升级：换镜像后重建容器，数据卷保留
docker compose pull            # 或重新 docker load 新镜像
docker compose up -d
```

## 8. 常见问题

| 现象 | 原因 |
| --- | --- |
| `set IMBO_SMTP_URL ... or IMBO_MAIL_LOG_ONLY=1` 后容器不停重启 | `.env` 里两个都没配。先设 `IMBO_MAIL_LOG_ONLY=1` |
| 手机能开网页但 App 登录失败 | App 里填的服务端地址必须是 https；http 只在 debug 包允许 |
| 推送收不到 | `.env` 的 `IMBO_NTFY_URL` 要指向 compose 里的 `http://ntfy:80`，不是 `127.0.0.1:8081` |
| ntfy 能收到但手机没通知 | 手机上 ntfy 客户端没订阅该话题，或话题名不一致 |
