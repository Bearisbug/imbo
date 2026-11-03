#!/bin/sh
# 一条命令起 imbo-server + ntfy。
#
#   ./setup.sh                 # 有 .env 就用，没有就从 .env.example 复制
#   ./setup.sh --tailscale     # 起完顺便配 tailscale serve（需要已登录 Tailscale）
#
# 前置：Docker（含 compose 插件）。脚本只做检查、起容器、等健康、打印下一步，
# 不会改系统配置，也不会装任何东西。
set -eu

cd "$(dirname "$0")"

say() { printf '%s\n' "$*"; }
fail() { printf '错误：%s\n' "$*" >&2; exit 1; }

# ---- 1. 检查 docker ----
command -v curl >/dev/null 2>&1 || fail "缺少 curl（脚本用它做健康检查）。Debian/Ubuntu：apt-get install -y curl"
command -v docker >/dev/null 2>&1 || fail "没装 docker。Debian/Ubuntu：curl -fsSL https://get.docker.com | sh"
docker compose version >/dev/null 2>&1 || fail "docker compose 插件缺失。装 docker-compose-plugin，或改用 docker-compose"
docker info >/dev/null 2>&1 || fail "docker 守护进程没在跑（试试 sudo systemctl start docker）"

# ---- 2. .env ----
if [ ! -f .env ]; then
  cp .env.example .env
  say "已从 .env.example 生成 .env。"
  say "  · 只想先跑通：保持 IMBO_MAIL_LOG_ONLY=1，验证码会打进日志"
  say "  · 要真发邮件：填 IMBO_SMTP_URL，并把 IMBO_MAIL_LOG_ONLY 改成 0"
fi

# 必填项检查：SMTP 和 log-only 至少有一个
if ! grep -Eq '^IMBO_SMTP_URL=.+' .env && ! grep -Eq '^IMBO_MAIL_LOG_ONLY=1' .env; then
  fail ".env 里既没有 IMBO_SMTP_URL，也没开 IMBO_MAIL_LOG_ONLY=1——服务端会启动失败。先设 IMBO_MAIL_LOG_ONLY=1 跑通。"
fi

IMBO_PORT=$(grep -E '^IMBO_PORT=' .env | tail -1 | cut -d= -f2)
NTFY_PORT=$(grep -E '^NTFY_PORT=' .env | tail -1 | cut -d= -f2)
IMBO_PORT=${IMBO_PORT:-8080}
NTFY_PORT=${NTFY_PORT:-8081}

# ---- 3. 起服务 ----
say "启动容器…"
docker compose up -d

# ---- 4. 等健康 ----
say "等待服务端就绪（首次启动要初始化内嵌 PostgreSQL，可能要十几秒）…"
i=0
while [ $i -lt 60 ]; do
  if curl -fsS -m 3 "http://127.0.0.1:${IMBO_PORT}/v1/health" >/dev/null 2>&1; then
    break
  fi
  i=$((i + 1))
  sleep 2
done

if ! curl -fsS -m 5 "http://127.0.0.1:${IMBO_PORT}/v1/health" >/dev/null 2>&1; then
  say "服务端没起来。看日志："
  docker compose logs --tail 30 imbo
  exit 1
fi

if ! curl -fsS -m 5 "http://127.0.0.1:${NTFY_PORT}/v1/health" >/dev/null 2>&1; then
  say "ntfy 没起来。看日志："
  docker compose logs --tail 30 ntfy
  exit 1
fi

say ""
say "服务端：http://127.0.0.1:${IMBO_PORT}/v1/health"
curl -fsS -m 5 "http://127.0.0.1:${IMBO_PORT}/v1/health" || true
say ""
say "ntfy：  http://127.0.0.1:${NTFY_PORT}/v1/health"
curl -fsS -m 5 "http://127.0.0.1:${NTFY_PORT}/v1/health" || true
say ""

# ---- 5. 可选：Tailscale ----
if [ "${1:-}" = "--tailscale" ]; then
  command -v tailscale >/dev/null 2>&1 || fail "没装 tailscale。装好并 tailscale up 后再跑一次 --tailscale"
  say "配置 tailscale serve…"
  tailscale serve --bg "http://127.0.0.1:${IMBO_PORT}"
  tailscale serve --bg --https=8443 "http://127.0.0.1:${NTFY_PORT}"
  say ""
  say "服务端地址：https://$(tailscale status --json | sed -n 's/.*"DNSName":"\([^"]*\)".*/\1/p' | head -1)"
  say "ntfy 地址：  同上但加 :8443"
  say "（手机装 Tailscale 并登录同一账号即可访问；Serve 未开启时会提示授权链接）"
else
  say "下一步（三选一）："
  say "  A. 公网 https（推荐给其他人用）：配好域名 + 反代/Funnel 后，App 里填 https 地址"
  say "  B. 私有访问：装 Tailscale 并登录，然后 ./setup.sh --tailscale"
  say "  C. 同一局域网：App 里填 http://<这台机器的局域网IP>:${IMBO_PORT}（仅 debug 包允许 http）"
fi

say ""
say "出包（在开发机上）："
say "  android/scripts/release.ps1 -BaseUrl <你的 https 地址> -NtfyUrl <ntfy 的 https 地址>"
