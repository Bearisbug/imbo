> 历史方案：安卓端已改走 ntfy（见 push-ntfy.md），本文留作参考，Firebase 依赖与 ImboMessagingService 已从工程移除。

# 安卓推送（FCM）配置

官方 iOS/Web 版走 Expo 推送中转；原生安卓拿的是 FCM 注册令牌，服务端原来只认
`ExponentPushToken[...]`，所以自建服务器要用推送，需要：

1. 一个 Firebase 项目（免费）；
2. 把 `google-services.json` 放进 `android/app/`（App 侧）；
3. 服务端配三个 `IMBO_FCM_*` 环境变量（发送侧）；
4. 服务端打上 FCM 补丁（本仓库 `imbo-server/src/fcm.rs` 已经包含）。

没有第 2 步时 App 照常构建运行，`BuildConfig.PUSH_ENABLED=false`，只是不注册令牌、收不到通知。没有第 3 步时服务端只会记一条 `push: FCM tokens registered but IMBO_FCM_* isn't configured`。

## 1. 建 Firebase 项目

1. 打开 <https://console.firebase.google.com>，新建项目（不需要 Google Analytics）。
2. 「添加应用」→ Android，包名填 `dev.imbo.android`（与我们的一致），下载 `google-services.json`。
3. 把文件放到 `android/app/google-services.json`，重新构建：

```bash
cd android
./gradlew :app:assembleDebug "-Pimbo.useMock=false" "-Pimbo.baseUrl=https://你的服务器"
```

4. 装到手机上，登录后应弹出「允许发送通知？」，允许即可。

## 2. 服务端拿发送凭据

1. Firebase 控制台 → 项目设置 → 「服务账号」→ 生成新的私钥，会下载一个 JSON。
2. JSON 里有 `project_id`、`client_email`、`private_key`（PEM 字符串，带 `\n`）。
3. 给容器加环境变量：

```sh
docker run -d --name imbo-local -p 8080:8080 -v imbo-data:/data \
  -e IMBO_LISTEN=0.0.0.0:8080 \
  -e IMBO_SMTP_URL='smtps://user:pass@smtp.example.com' \
  -e IMBO_FCM_PROJECT_ID='your-project-id' \
  -e IMBO_FCM_CLIENT_EMAIL='firebase-adminsdk-xxxxx@your-project-id.iam.gserviceaccount.com' \
  -e IMBO_FCM_PRIVATE_KEY='-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----\n' \
  ghcr.io/bearisbug/imbo-server:latest
```

`IMBO_PRIVATE_KEY` 里的换行既可以直接是 `\n` 两字符，也可以是真实换行，服务端会自己处理（和 Waffo/Apple 的私钥同一个解析函数）。三个变量要么都设、要么都不设，只设一半会启动失败并说明缺哪个。

## 3. 验证

```bash
# 服务端日志里应出现 push_sent 且 provider="fcm"
docker logs -f imbo-local | grep push

# 手机锁屏后，用另一台设备或 curl 给自己发一条消息
```

App 侧收到后：前台走 `ImboMessagingService.onMessageReceived` 自己弹通知；后台系统直接展示 `notification`，点开由 `MainActivity` 读 `conversation_id` 跳到对应会话。

## 4. 排错

| 现象 | 检查 |
| --- | --- |
| 登录后没弹通知授权 | Android 13+ 才有该弹窗；已拒绝过要去系统设置里重新打开 |
| 服务端日志没有 `push_sent` | 手机是否注册了令牌：`SELECT push_token FROM app_sessions WHERE revoked_at IS NULL;` |
| 日志有 `FCM tokens registered but IMBO_FCM_* isn't configured` | 服务端三个环境变量没配全 |
| 日志 `FCM refused: 401/403` | 服务账号私钥与 project_id 不匹配，或 FCM API 没启用 |
| 日志 `FCM refused: 404` | 令牌失效（App 卸载/重装），服务端会顺手把该令牌清掉 |
| 收到通知但点开没进会话 | 检查服务端 `data.conversation_id`；客户端读的是字符串 extra |
