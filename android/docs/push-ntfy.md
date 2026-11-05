# 用 ntfy 做推送（自建，替代 FCM）

FCM 在国内网络到不了 Google，这条链路走不通。ntfy 是自建推送服务：服务端把通知 POST 到话题，手机上的 ntfy 客户端订阅话题并弹出通知。不需要 Google，不走厂商通道。

## 1. 起一个 ntfy 实例

```bash
docker run -d --name ntfy -p 8081:80 binwiederhier/ntfy serve
```

公网或 Tailscale 上暴露时建议先开访问控制（`ntfy user add` / `ntfy access`），否则知道话题名的人都能读通知——**话题名就是密码**。

## 2. 让 imbo-server 知道 ntfy 在哪

```bash
docker run -d --name imbo-local -p 8080:8080 \
  --add-host=host.docker.internal:host-gateway \
  -e IMBO_LISTEN=0.0.0.0:8080 \
  -e IMBO_SMTP_URL='smtps://…' \
  -e IMBO_NTFY_URL=http://host.docker.internal:8081 \
  -e IMBO_NTFY_TOKEN='tk_…'          # 开了访问控制才需要，公共实例留空
  -v imbo-fcm-data:/data \
  imbo-server:ntfy
```

- `IMBO_NTFY_URL` 是 ntfy 的**基础地址**，服务端会拼成 `{base}/{topic}`。
- 和 ntfy 不同网络时换成 Tailscale 里的地址，例如 `http://100.x.y.z:8081` 或 `https://ntfy.你的域名`。
- 没配 `IMBO_NTFY_URL` 而 App 注册了 `ntfy:` 令牌时，服务端会打一条 warn 日志，不会静默丢。

## 3. App 端做什么

App 第一次登录后生成一个本机话题（`imbo-` + 12 位十六进制，由 `ANDROID_ID` 与包名派生，存在 `imbo_ntfy` 偏好里），把 `ntfy:<topic>` 注册到服务端。账号页「推送」一节显示话题，点「复制」可以得到订阅链接。

- 话题由 `ANDROID_ID` 派生，**重装后不变**，ntfy 客户端订阅一次就长期有效；换手机或恢复出厂才会变。
- 退出登录会注销令牌，这台手机不再收旧账号的通知。

## 4. 手机上订阅

1. 装 ntfy 客户端（F-Droid / GitHub Releases / 应用商店都有）。
2. 添加订阅，地址填账号页复制出来的那个，例如 `http://10.0.0.5:8081/imbo-xxxxxxxxxxxx`。
3. 手机和 ntfy 实例要能互通：同一局域网、或都加入同一个 Tailscale 网络。

## 5. 验证

服务端日志应出现：

```
INFO push event="push_sent" provider="ntfy" message_id=53 account_id=79830691 tokens=1
```

真机验收（2026-11-03，Xiaomi 23113RKC6C）：手机装 ntfy 客户端、订阅 `http://<电脑>:8081/imbo-xxxxxxxxxxxx` 后，服务端发一条消息，通知栏出现 `bob · 标题正文都对了`；群里被 @ 时的通知标题是群名、正文是 `bob @了你：…`。

> 服务端发布用的是 ntfy 的 **JSON 根路径**接口（`POST {base}` + `{"topic":…,"title":…,"message":…}`）。
> 打到 `POST {base}/{topic}` 带 JSON 时，ntfy 会把整个 JSON 当正文，通知里就是一串花括号——这个坑踩过一次。

在电脑上订阅同一话题也能看到原始载荷：

```bash
curl -sN http://127.0.0.1:8081/imbo-xxxxxxxxxxxx/json
# {"id":"…","event":"message","topic":"imbo-xxxxxxxxxxxx",
#  "message":"{\"message\":\"ntfy 推送验证\",\"priority\":3,\"tags\":[\"speech_balloon\"],\"title\":\"bob\"}"}
```

## 6. 排障

| 现象 | 先查什么 |
| --- | --- |
| 服务端日志是 `push: ntfy tokens registered but IMBO_NTFY_URL isn't configured` | 容器没带 `IMBO_NTFY_URL` |
| `push: ntfy refused` | `IMBO_NTFY_TOKEN` 与 ntfy 的访问控制不匹配；或话题名非法 |
| `push: ntfy unreachable` | 容器里解析不到 ntfy 地址；用 `host.docker.internal` 或 Tailscale IP，别用 `127.0.0.1` |
| 服务端发送成功、手机没弹 | ntfy 客户端没订阅该话题、话题拼错、或手机到 ntfy 实例不通 |
| 通知来了但点不开会话 | 载荷里的 `click` 是 `imbo://chat/<会话id>`，需要 App 注册了该 scheme；MIUI 首次会问「ntfy 想要打开 Imbo」，选「始终允许」 |
