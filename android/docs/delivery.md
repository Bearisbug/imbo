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

## 6. 发布与 CI

发布包在开发机上出（`scripts/release.ps1`），签名密钥不进仓库也不进 CI。仓库里的
`.github/workflows/android-ci.yml` 只做两件事：跑单测、构建未签名的 debug APK。

```bash
cd android
./scripts/release.ps1 -BaseUrl https://imbo.example.com -NtfyUrl https://ntfy.example.com
./scripts/verify-apk.ps1 ../dist/imbo-release-signed.apk -BaseUrl https://imbo.example.com
```

复现清单（工具链版本、期望输出）见 [`configuration.md`](configuration.md) 的「6b. 复现清单」。

> 如果以后确实要 CI 出包，需要把 keystore 放进 GitHub secrets，并按 `configuration.md` 第 6 节改回 release workflow——默认不这么做，因为等于把签名密钥交给第三方保管。
## 9. 推送（ntfy）

FCM 在国内到不了 Google，当前实现改走自建 ntfy。部署与排障见 [`push-ntfy.md`](push-ntfy.md)：起 ntfy 容器 → 服务端带 `IMBO_NTFY_URL` → App 账号页复制订阅链接 → 手机 ntfy 客户端订阅。