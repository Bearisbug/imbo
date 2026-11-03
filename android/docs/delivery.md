# Imbo 安卓端交付说明

这份文档面向「把这套 App 装到手机上、连上自己的服务器、并能长期维护」的人。功能清单与验证记录见 `README.md`，自建服务端与跨网络访问见 `docs/self-host-android.md`，推送见 `docs/push-fcm.md`。

## 1. 包里有什么

| 文件 | 用途 |
| --- | --- |
| `dist/imbo-release-signed.apk` | 正式签名 + R8 混淆的 release 包（约 2.5 MB），发给别人装这个 |
| `dist/imbo-release.aab` | 同上，AAB 格式，将来上 Google Play 用 |
| `dist/imbo-real-server-debug.apk` | 调试包，内置 `BASE_URL` 指向本机局域网服务器，联调用 |
| `dist/imbo-mock-demo-debug.apk` | mock 数据包，不连任何服务器，装上就能把全部页面点一遍 |

release 包默认连 `https://imbo.example.com`（占位地址）。正式发版前用脚本重新构建（带签名、算校验和、自动拷到 `dist/`）：

```powershell
./scripts/release.ps1 -BaseUrl https://imbo.xxx.ts.net -NtfyUrl https://ntfy.xxx.ts.net
```

脚本内部走的就是下面这条 gradle 命令，手动执行也可以：

```bash
cd imbo/android
./gradlew :app:assembleRelease :app:bundleRelease \
  "-Pimbo.useMock=false" "-Pimbo.baseUrl=https://你的域名"
```

装到手机后也可以在 App 里改：登录页底部「服务器 · …」或账号页「服务器」一节。改了会强制退出登录（token 属于旧服务器）。

## 2. 装机步骤

1. 手机上允许「安装未知来源应用」。
2. `adb install -r dist/imbo-release-signed.apk`，或把 APK 传到手机点开安装。
3. 打开 App，确认登录页底部显示的服务器地址正确；不对就点它改。
4. 输入邮箱 → 收验证码 → 登录。新账号会先要填名字。
5. 登录后系统会问「允许发送通知？」，允许（没有推送通道时允许了也收不到，见第 5 节）。

## 3. 服务端要准备什么

最小可用（局域网自用）：

```sh
docker run -d --name imbo-local -p 8080:8080 -v imbo:/data \
  -e IMBO_LISTEN=0.0.0.0:8080 \
  -e IMBO_SMTP_URL='smtps://user:pass@smtp.example.com' \
  ghcr.io/bearisbug/imbo-server:latest
```

- **不配 `IMBO_SMTP_URL` 就没有人收得到验证码**（本地开发可用 `IMBO_MAIL_LOG_ONLY=1`，验证码打在 `docker logs` 里）。
- 公网 HTTPS：配 `IMBO_DOMAIN` 和 `IMBO_ACME_EMAIL`，服务端自己申请证书；或者按 `docs/self-host-android.md` 用 Tailscale/隧道。
- 注册范围可用 `IMBO_SIGNUP_EMAIL_DOMAINS` 限定。

## 4. 发布前检查清单

- [ ] `versionCode` 比上一版大（现在时 `2`），`versionName` 是准的（`1.0.0`）
- [ ] `-Pimbo.baseUrl` 填的是真实域名，且该域名走 HTTPS

- [ ] `android/keystore.properties` 指向正式 keystore，口令正确
- [ ] `./gradlew :app:assembleRelease :app:bundleRelease` 通过
- [ ] release 包装到真机，登录、发消息、发图、群管理各走一遍（R8 只有在 release 包才会暴露问题）
- [ ] `apksigner verify --print-certs` 的输出与预期证书一致

签名密钥与口令由项目负责人保管，**不入库**（`.gitignore` 已排除 `keystore.properties`、`*.jks`）。丢失 keystore 意味着同包名无法再发更新，务必离线备份两份。

## 5. 已知限制

| 项 | 现状 | 影响 |
| --- | --- | --- |
| 推送 | 已改走 ntfy：服务端发到话题、手机端用 ntfy 客户端订阅；服务端链路已验证（`push_sent provider="ntfy"`） | 需要装 ntfy 客户端并订阅本机话题；话题名即密码，公网实例要开访问控制 |
| 公网访问 | 走 Tailscale + `tailscale serve`（见 `deploy-tailscale.md`）；VPS 部署见 `imbo/deploy/docker/README.md`；App 内地址可改 | 需要一台常驻机器；关机即断 |
| 订阅内购 | 未做，服务端 billing 关闭 | 账号页显示「订阅暂未开放」 |
| @ 提及的通知 | 依赖推送通道 | 与推送同一条限制 |
| 多选分享图 | 纯 Canvas 绘制，没有单测 | 改排版时只能靠人眼看 |
| 群会话选择 | 只在账号真有 agent 会话时出现 | 纯手机账号看不到这一节 |

## 6. CI 发布（可选）

`.github/workflows/android-release.yml`：打 `android-v*` tag 或手动触发。

仓库需要配置：

| 类型 | 名称 | 内容 |
| --- | --- | --- |
| secret | `IMBO_KEYSTORE_BASE64` | `base64 -w0 imbo-release.jks` |
| secret | `IMBO_KEYSTORE_PASSWORD` | store 口令 |
| secret | `IMBO_KEY_PASSWORD` | key 口令（通常同上） |

| variable | `IMBO_BASE_URL` | 发布包内置的服务端地址 |
| variable | `IMBO_NTFY_URL` | 可选，ntfy 实例地址；不配则账号页只显示话题不显示订阅链接 |

产物在 workflow 的 artifacts 里：APK、AAB、`checksums.txt`。

## 7. 排障速查

| 现象 | 先查什么 |
| --- | --- |
| 登录时「连不上服务器」 | 手机浏览器打开 `http://服务器:8080/v1/health`，应返回 `{"status":"ok"}`；VPN/代理是否劫持了局域网 |
| 验证码收不到 | 服务端是否配了 SMTP；本地开发看 `docker logs` 里的 `mail-log-only code=…` |
| 发出去的消息对方没收到 | 双方是否联系人；服务端 `/v1/messages` 是否 201 |
| 收不到实时消息 | `/v1/events` 的 WebSocket 是否被反代/隧道断开 |
| 通知不弹 | 系统是否允许通知；推送通道是否配好（见第 5 节） |
| App 闪退 | `adb logcat | grep AndroidRuntime`；release 包看混淆后的类名是否缺 keep 规则 |
## 8. 验收截图索引

真机（Xiaomi 23113RKC6C / Android 16）截图都在 `../shots/delivery/`：

| 截图 | 证明的事 |
| --- | --- |
| `02-login` / `03-email` / `04-code` | 登录三步，中文界面，验证码页面 |
| `07-group-info` / `08-group-avatar` | 群信息页与群头像上传结果 |
| `10-remove-contact` / `11-contact-removed` | 联系人长按删除与删除后的列表 |
| `12-email-step1` / `13-email-step2` / `14-email-changed` | 换邮箱两步与换完后的账号页 |
| `15-delete-dialog` / `16-after-delete` | 删除账号确认与回登录页 |
| `17-signout` / `18-account` | 退出账号、账号页访问状态 |
| `19-group-info-clean` | 群信息页顶栏不再重复群名 |
| `20-server-dialog` | 服务器地址修改对话框 |
| `21-group-sessions` / `38-group-info-scroll` / `42-session-toggle` | 「我的会话」一节与勾选效果 |
| `22-attach-menu` / `34-camera` / `37-camera-sent` | 拍照入口、系统相机、拍照发出的图片消息 |
| `29-multiselect` / `30-share-sheet` | 多选消息与分享为图片的系统面板 |
| `31-mention-sent` | @ 提及的发送结果 |
| `43-retry-failed` / `45-net-error` | 发送失败的重试提示与网络错误中文化 |
| `46-sessions-lastseen` | 会话列表「在线 / 最后在线 HH:mm」 |
| `47-image-viewer` | 图片全屏查看 |
| `28-release-launch` / `32-release-latest` | R8 + 正式签名的 release 包能正常启动 |
## 9. 推送（ntfy）

FCM 在国内到不了 Google，当前实现改走自建 ntfy。部署与排障见 [`push-ntfy.md`](push-ntfy.md)：起 ntfy 容器 → 服务端带 `IMBO_NTFY_URL` → App 账号页复制订阅链接 → 手机 ntfy 客户端订阅。