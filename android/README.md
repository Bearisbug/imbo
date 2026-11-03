# Imbo 安卓端（Kotlin + Compose）

14 个页面全部可用，交互接通，mock 与真实服务端共用同一套契约，103 个单元测试覆盖数据通道与状态机。

各功能当前是「已实现 / 已验证 / 未验证 / 阻塞」的明细见 [`docs/status.md`](docs/status.md)；交付、装机、服务端准备、发布检查清单见 [`docs/delivery.md`](docs/delivery.md)；自建与跨网络见 [`docs/self-host-android.md`](docs/self-host-android.md)、一键 Tailscale 流程见 [`docs/deploy-tailscale.md`](docs/deploy-tailscale.md)；推送：Android 端走 ntfy（自建，Firebase/FCM 依赖已移除），见 [`docs/push-ntfy.md`](docs/push-ntfy.md)；FCM 的历史方案留在 [`docs/push-fcm.md`](docs/push-fcm.md)。

## 构建与安装

两个产物，按用途选一个装（同一台手机请先卸载另一个，否则签名相同但数据通道不同会互相覆盖）：

```bash
cd imbo/android

# 1) 连真实服务端（地址在构建时固定，运行时也能在 App 里改）
./gradlew :app:assembleDebug "-Pimbo.useMock=false" "-Pimbo.baseUrl=http://<你的服务端>:8080"

# 2) 纯 mock 演示，不依赖任何服务端
./gradlew :app:assembleDebug

# 3) 发布包：正式签名 + R8 混淆 + 资源压缩（约 2.5 MB）
./gradlew :app:assembleRelease :app:bundleRelease "-Pimbo.useMock=false" "-Pimbo.baseUrl=https://<你的域名>"

adb install -r app/build/outputs/apk/debug/app-debug.apk
```

发布签名从 `android/keystore.properties` 读（`storeFile` / `storePassword` / `keyAlias` / `keyPassword`），这个文件已 gitignore；没有它时 release 仍可构建，只是不签名，方便别人 fork 后直接跑。CI 见 `.github/workflows/android-release.yml`：打 `android-v*` tag 触发，需要的 secrets 写在该文件开头。

仓库根目录 `dist/` 放了两份已构建好的：`imbo-real-server-debug.apk`（连 `http://10.100.135.176:8080`）和 `imbo-mock-demo-debug.apk`（mock 数据，装上即可走完所有页面）。

本机（Windows）用的环境变量：

```powershell
$env:JAVA_HOME="C:\DevTools\jdk-17"; $env:GRADLE_USER_HOME="C:\DevTools\gradle-home"; $env:ANDROID_HOME="C:\DevTools\android-sdk"
```

## 本地起服务端做真机联调

> 手机连着电脑、又开着代理/VPN 导致连不上局域网时，用 USB 反向端口映射绕过网络：
> `adb reverse tcp:8080 tcp:8080`，然后在 App 的登录页或账号页把服务器地址改成 `http://127.0.0.1:8080`。
> 这样流量走 USB，和 WiFi、VPN、防火墙都无关。注意 `adb reverse` 在设备重连后会丢，重连后再执行一次。


服务端是 `imbo` 仓库里的 `imbo-server`（Apache-2.0），自带 PostgreSQL 的单个镜像，本地跑不需要域名和证书：

```bash
docker run -d --name imbo-local -p 8080:8080 \
  -e IMBO_LISTEN=0.0.0.0:8080 \
  -e IMBO_DEV=1 \
  -e IMBO_MAIL_LOG_ONLY=1 \
  -e IMBO_PUSH_LOG_ONLY=1 \
  -e IMBO_REVIEW_EMAIL=alice@imbo.test \
  -e IMBO_REVIEW_CODE=123456 \
  -v imbo-local:/data \
  ghcr.io/bearisbug/imbo-server:latest
```

- 不设 `IMBO_DOMAIN` → 纯 HTTP，手机用 `http://<电脑局域网IP>:8080` 直连。
- 不设 `IMBO_BILLING_START` → 不收费，账号永远有效；`/v1/me` 返回 `access.state = "free"`。
- `IMBO_REVIEW_EMAIL/CODE` 让这个邮箱的验证码固定为 `123456`，不用去日志里翻。
- 造数据（只在容器内可行，接口限制 loopback）：
  ```bash
  docker exec imbo-local imbo-server dev seed --homes /tmp/homes \
    --names alice,bob,carol --contacts alice:bob,alice:carol \
    --server http://127.0.0.1:8080
  ```
  账号邮箱是 `<name>@imbo.test`，bob/carol 的设备 token 在 `/tmp/homes/<name>/credentials`。
- Windows 要放行 8080 入站，手机与电脑同一局域网。

把 App 指过去有两种方式：

```bash
# 1) 构建时固定
./gradlew :app:assembleDebug -Pimbo.useMock=false -Pimbo.baseUrl=http://192.168.1.5:8080
```
2）debug 包在登录页**长按 wordmark** 打开「Server」对话框输入地址，立即生效、不用重新构建（值存在 `imbo_debug` prefs，release 构建忽略）。

> 明文 HTTP 只在 debug 构建允许（`app/src/debug/AndroidManifest.xml`），release 仍只接受 HTTPS。

## 两条数据通道

界面只认 `ImboApi`，切换只发生在 `di/AppModule.kt` 一处。

默认走 mock，不依赖服务端：

```bash
./gradlew :app:assembleDebug                      # imbo.useMock 默认 true
./gradlew :app:assembleDebug -Pimbo.useMock=false -Pimbo.baseUrl=https://api.imbo.dev
```

真实通道（`data/remote/`）对应 imbo-server 的端点：登录 `POST /v1/app/session`（platform 固定 `android`）、`GET|PATCH /v1/me`、会话列表与消息、发送、`/v1/agents/{id}/messages|read`、联系人、请求、设备、屏蔽、举报、群、账单。发送地址按服务端规则拼：单聊用对方账号 ID，群聊用 `#群ID`，自己的 agent 会话用 `账号ID/短ID`（会话在界面上拿一个稳定的负 id，沿用同一个聊天页）。

`/v1/events` 只推 `{"type":"changed","area":"…"}`，客户端收到后按区域重新拉取，不做本地补丁；未知区域按全量刷新处理，断线退避重连。

## 用 mock 数据验收

调试构建支持用 intent 参数直接落到某一屏并注入场景（正式构建里不生效）：

```bash
# 路由：login / code / register / welcome / chats / sessions / contacts /
#       new-group / add-contact / account / blocked / chat/<id>
adb shell am start -n dev.imbo.android/.MainActivity --es route chats

adb shell am start -n dev.imbo.android/.MainActivity --es route chats --ez mock_empty true
adb shell am start -n dev.imbo.android/.MainActivity --es route chats --es mock_error 401
adb shell am start -n dev.imbo.android/.MainActivity --es route chats --es mock_latency 5000

# 应用内语言
adb shell cmd locale set-app-locales dev.imbo.android --locales zh-CN
```

`mock_error` 取 `401` / `500` / `timeout`；`mock_latency` 是毫秒。发消息时 mock 会在 600ms 后回一条 `Got it.`，用来验证刷新链路。登录用任意 6 位数字验证码，`111111` 会走到"验证码错误"分支。

账号页底部有 debug 专用的 `Mock scenario` 面板，可在应用内切延迟、错误、空数据。

## 测试

```bash
# 直接跑；工程路径含中文时会自动镜像到 ASCII 目录再跑（原因见下）
powershell -ExecutionPolicy Bypass -File scripts/run-tests.ps1
```

97 个 JVM 单测，全部通过：

| 文件 | 数量 | 覆盖 |
| --- | --- | --- |
| `MockImboApiTest` | 31 | mock 通道的种子数据、未读、拉黑隐藏与恢复、验证码校验、建群与群管理（改名/邀请/移除/转让）、附件与头像存取、换邮箱两步校验、群头像与群主判定、错误与空数据注入 |
| `RemoteImboApiTest` | 26 | 用 MockWebServer 断言真实客户端的请求路径/请求体/响应映射：登录、鉴权头、会话映射、单聊与群聊地址、agent 会话负 id、附件裸流上传（`Imbo-File-Name` / `Imbo-File-Sha256`）、头像原始字节 PUT、群头像 PUT 与绝对地址、换邮箱两步、注销账号的 `{"confirm":"DELETE"}`、404 空页、Problem 错误、举报原因映射、登出清 token |
| `ViewModelTest` | 30 | 发送的乐观回显→sent/failed、附件乐观上屏、空消息不发、打开 agent 会话标记已读、认证三态、加联系人、建群、群信息写状态、头像 pending/失败、取消屏蔽 |
| `ServerConfigStoreTest` | 5 | 服务器地址整理：补 https、保留端口/路径前缀、去尾斜杠、拒绝空白与非 http(s) 协议 |
| `ErrorTextTest` | 5 | 服务端/网络错误文案到本地化串的映射：联系人、登录失效、限流、群主、网络不可达、超时、未知原样 |

为什么要有 `scripts/run-tests.ps1`：这台机器工程路径含中文，JDK 17 的测试 worker 按 ANSI 代码页解析 classpath，会把测试类解析成不存在的路径（报 `ClassNotFoundException`）。脚本检测到非 ASCII 路径就镜像到 `C:\Temp\imbo-android-tests` 再跑。ASCII 路径下 `./gradlew :app:testDebugUnitTest` 可直接跑。

## 已实现的界面与行为

- 认证：登录 → 验证码（错误留在原地、可重发）→ 欢迎页（带名字）→ 对话列表；注册走同一条验证码路径，名字进账号。
- 对话列表、会话列表（agent）、联系人（含请求接受/拒绝）、搜索入口（会话标题与最近消息本地过滤）、新建群（选成员后建群并进入群聊天）。
- 聊天：日期分隔、气泡、乐观发送、失败转 error 并留在原位、键盘弹出时输入区上移、发送后清空；头部搜索在对话内过滤消息。
- 账号：改名（PATCH `/v1/me`）、账号 ID、套餐与订阅入口、更换/移除头像、换邮箱（两步：验证码发到新邮箱再换）、删除账号（`DELETE /v1/account`，正文带 `{"confirm":"DELETE"}`，成功后回登录页）、语言切换（应用内生效并持久化）、设备列表与退出、屏蔽名单、注销/退出、举报与拉黑。
- 附件：输入栏 `+` 可选**拍照**（`TakePicture` + FileProvider，缓存到 `cache/camera`）、照片（系统 Photo Picker）或文件（OpenDocument），上传后发送；气泡里图片用 Coil 渲染，其他类型显示文件名与大小。上传走服务端的裸流协议（`Imbo-File-Name` 百分号编码 + `Imbo-File-Sha256`），不是 multipart。
- 群管理：聊天页「更多」→ 群信息，可看成员与群主标记、换/移除群头像（仅群主，点与长按）、改群名、邀请联系人、移除成员、转让群主（仅群主）、**选哪些自己的会话接收本群消息**（`PUT /v1/groups/{id}/sessions`）、退群、解散群（后两者二次确认）；写操作有 pending 与失败提示，改完群名返回聊天页标题会同步，群顶栏也显示群头像。
- 联系人：长按删除（`DELETE /v1/contacts/{account_id}`，二次确认，说明聊天记录保留）；添加支持**账号 ID 或邮箱**两种方式。
- 会话：agent 会话长按重命名（`PUT /v1/agents/{id}/title`），只改自己这侧的显示名；列表里显示「工具 · 所在电脑 · 在线/离线」。
- 发布：`assembleRelease` 走正式签名 + R8 混淆 + 资源压缩（2.5 MB），keep 规则覆盖 kotlinx.serialization、OkHttp、Coil、Hilt、Firebase；tag 触发 GitHub Actions 出 APK+AAB 并附 SHA-256。
- 消息：气泡元信息显示来源（安卓 App / 网页 / 电脑终端，来自服务端 `via`）；长按可复制、引用、转发、多选；点图片看全屏大图（点任意位置关闭）；往上翻历史时会浮出一个「回到最新消息」的圆钮，点一下滚回底部；转发会把正文和附件原样发到另一个会话（附件是服务端文件 id，不用重新上传）；多选后可把几条消息画成一张图交给系统分享（`ShareImage` 用离屏 Canvas 绘制，走 FileProvider）；发送失败的气泡显示「发送失败 · 点按重试」，点一下重发。
- 错误文案：服务端返回的英文 detail 会映射成本地化说法（已是联系人、登录失效、频率限制、非群主等），网络类的 `Failed to connect` / `unexpected end of stream` / 超时也换成中文；没命中才原样显示，不吞失败原因。
- 登录页会记住上次用的邮箱并预填（只存邮箱，不含凭据）。
- 自己的消息统一显示「我」，不再一半账号名一半 `me`；离线会话显示「最后在线 21:04」这类时间。
- 版本：`versionName 1.0.0` / `versionCode 2`。
- 群聊 @ 提及：输入 `@` 弹出群成员选择框，选中后插入 `@名字`，消息里加粗显示；被点名的人由服务端单独推送（`mention`）。
- 服务器：登录页与账号页都能改服务端地址（`ServerConfigStore`），release 也生效、只放 https；换地址后要求重新登录。自建与跨网络步骤见 `docs/self-host-android.md`。
- 推送：Android 13+ 运行时申请通知权限，登录后在对话列表注册 FCM 令牌（`PUT /v1/app/push`），通知点开直达会话。**没有 `google-services.json` 时整条链路是空操作**，App 照常可用。
- AI 合规：agent 会话头部常驻「AI agent」标识，副标题说明回复来自哪个 AI；「更多」→ 举报（3 步内到原因选择，可同时拉黑）。

设计 token 取自官方线上站点：背景 `#0F0F0E`、主文字 `#EDEAE2`、对方气泡 `#312F2B`、说明 `#A19D93`、凹槽 `#080707`、边框 `#838077`、未读红 `#D91E36`。字号 22/14/13/12sp，品牌栏 48dp、tab 行 40dp、列表行 58dp、头像 32dp、气泡圆角 8dp。字体是打包的 JetBrains Mono（OFL，见 `NOTICE`），中文按字形回落系统，与官方 Web 一致。产品只有暗色主题，不做亮色分支。

## 验收证据（`shots/`，模拟器 Pixel 5 / API 35 实拍；`shots/delivery/` 为真机 Xiaomi 23113RKC6C / Android 16）

| 截图 | 证明的事 |
| --- | --- |
| `login / code / register / welcome` | 认证四屏 |
| `delivery/02-login` `delivery/03-email` `delivery/04-code` | 真机登录三步（中文界面、验证码已发到邮箱） |
| `delivery/07-group-info` `delivery/08-group-avatar` | 群信息页；群头像从占位变成刚选的照片 |
| `delivery/10-remove-contact` `delivery/11-contact-removed` | 长按联系人 → 删除确认 → 列表只剩 carol |
| `delivery/12-email-step1` `delivery/13-email-step2` `delivery/14-email-changed` | 换邮箱两步对话框与换完后的账号页 |
| `delivery/15-delete-dialog` `delivery/16-after-delete` | 删除账号确认 → 回登录页 |
| `delivery/17-signout` `delivery/18-account` | 退出这个账号、账号页访问状态文案 |
| `chats / sessions / contacts / account / blocked / new-group / add-contact` | 七个主要页面 |
| `chat-1 / chat-2 / chat-5` | 单聊 / 群聊 / agent 会话（带 AI 标识） |
| `state-empty / state-error / loading2` | 空态、401 错误态、慢网加载态 |
| `zh-account` | 中文文案 |
| `send3` | 键盘弹出后发送：气泡上屏、输入框清空、600ms 后收到回复 |
| `blocked-after` | 拉黑后对话消失、未读从 5 降到 3 |
| `font15-chats` | 1.5 倍系统字号下列表仍完整渲染 |
| `chat-search` | 聊天内搜索：输入 `customer_id` 后只剩匹配的那条 |

## 真实服务端已验证的部分（本机 docker + 真机）

在 `ghcr.io/bearisbug/imbo-server:0.2.17` 上逐项跑过：

| 链路 | 结果 |
| --- | --- |
| 登录（`alice@imbo.test` + 固定验证码） | 通过，token 落盘，冷启动直接进对话列表 |
| 联系人 | 通过，种子里的 bob / carol 都在 |
| 单聊发送 | 通过，服务端消息正文与客户端一致 |
| 收消息 + 实时刷新 | 通过，bob 用 curl 发的回复不刷新页面就出现在聊天里（`/v1/events`） |
| 附件上传 | 通过，`/data/files` 有文件，`Imbo-File-Sha256` 校验通过 |
| 附件下载 | 通过，图片落到 `files/attachments` 并在气泡里渲染 |
| 头像上传 | 通过，`/v1/me` 返回 `avatar_url` |
| 建群 + 群内发首条消息 | 通过，服务端生成群会话（`kind: group`），消息 `via: app` |
| 群邀请的接受 | 通过，bob 登录后在联系人里看到「邀请你加入「ship room」」，接受后服务端群成员变成 2 |
| 消息长按 | 通过，弹出「复制 / 引用 / 取消」，引用会把 `> 原文` 塞进输入框 |
| 草稿 | 通过，退出对话再进来未发送的文字还在（按对话 id 存 prefs） |
| 历史消息分页 | 通过，57 条消息的会话滚到顶会加载更早的一页，首条 `hello from bob` 能翻出来 |

下面这几项在真机上点了完整路径（截图在 `shots/delivery/`），服务端状态用接口复核过：

| 链路 | 结果 |
| --- | --- |
| 删除联系人（长按 → 确认） | 通过，列表只剩 carol，服务端联系人同样只剩 carol；`/v1/requests` 重新加回后恢复 |
| 转让群主（群信息 → 转让给 bob → 确认） | 通过，页面上 bob 变「群主」、alice 变「你」，转让入口消失、退群解锁；服务端 owner 同步为 bob |
| 群头像（群主点头像 → 系统相册 → 选中） | 通过，头像立即变成所选照片，服务端 `avatar_url` 更新且图片可下载 |
| 换邮箱（账号页 → 更换 → 两次验证码） | 通过，第一步发码后弹第二步，填码后账号页显示新地址；服务端记录 `email changed` |
| 删除账号（账号页 → 删除账号 → 确认） | 通过，回到登录页，服务端日志出现 `account_deleted` |
| 退出这个账号 | 通过，账号页底部一行，点了回登录页 |
| 老账号再次登录 | 通过，直接进对话列表，不再出现「欢迎加入」注册欢迎页 |
| 群信息页跟随服务端变化 | 通过，页面开着时用接口把群主改成 bob，1 次刷新内页面自己更新（`/v1/events` → `ChangeArea.Conversations`） |
| 服务器地址可配置 | 通过，登录页底部与账号页各一处；改成局域网地址后立刻按新地址登录，换地址会强制重新登录 |
| 邮箱加联系人 | 通过，填 `carol@imbo.test` 发出请求（对方接受后回到联系人列表）；已是联系人时服务端原因原样显示 |
| 拍照入口 | 通过，`+` 面板出现「拍照 / 照片 / 文件」；相机需要真机授权，本轮只验到入口与 FileProvider 配置 |
| 拍照全链路（+ → 拍照 → 快门 → 确认） | 通过，相机返回后自动上传并发送；服务端该消息带 `files[0]`，文件名 `imbo-…jpg`、大小 3.7 MB |
| 群 @ 提及 | 通过，输入 `@` 弹出「提醒谁」列出成员，选 @bob 后发送；服务端该群最后一条消息正文为 `@bob`。成员刷新过：bob 是打开页面之后才接受邀请的，也出现在列表里 |
| 多选消息分享图 | 通过，长按 → 多选 → 选两条 → 分享为图片，系统分享面板出现 `imbo-….png` 预览 |
| 群会话选择 | 通过（mock），群信息页「我的会话」列出 checkout / docs-site，点一下选中圈变实心 |
| 消息来源标记 | 通过，群聊气泡元信息显示「alice · 01:52 · 安卓 App」 |
| 通知权限 | 通过，首次进对话列表弹「允许 Imbo 发送通知？」，授权后 `POST_NOTIFICATIONS` 生效 |
| ntfy 推送（服务端） | 通过，服务端日志 `push event="push_sent" provider="ntfy" message_id=53 tokens=1`；订阅端收到 `{"title":"bob","message":"标题正文都对了","priority":3}` |
| 发布包（R8 + 正式签名） | 通过，`app-release.apk` 2.48 MB、`app-release.aab` 5.07 MB，装到真机能正常启动登录页 |
| 错误文案本地化 | 通过，重复加 carol 时界面显示「对方已经在你的联系人里了。」而不是服务端英文 |
| 发送失败重试 | 通过，停掉服务端后发送 → 气泡上方出现「发送失败 · 点按重试」；恢复服务端后点该气泡，服务端收到重发的那条（`message_id=14`） |
| 网络错误本地化 | 通过，服务端停机时发送，提示行显示「连不上服务器，检查网络或服务器地址。」，不再是 `unexpected end of stream…` |
| 记住上次邮箱 | 通过，退出登录后登录页邮箱已预填 `alice@imbo.test` |
| 转发消息 | 通过，群聊长按「group seed」→ 转发 → 选 bob → 服务端 bob 的会话里出现同一正文（`message_id=17`） |
| 会话搜索 | 通过，点「搜索」输入 `bob` 只剩 bob 会话；输入 `zzz` 显示「没有匹配的会话」与说明文案 |
| 时间标签本地化 | 通过，中文界面里显示「昨天」而不是 `Yesterday` |
| 验证码重发倒计时 | 通过，验证码页显示「重新发送验证码（54 秒）」并置灰，到点变回可点的「重新发送验证码」 |
| 点联系人进会话 | 通过，联系人页点 bob 直接进入与 bob 的聊天；没聊过的人提示「发第一条消息就会开始会话」 |
| 图片全屏查看 | 通过，群聊里点图片附件打开全屏大图（黑底、等比缩放），点任意位置关闭回到聊天 |
| 未读分隔线 | 通过，收到 2 条未读后进会话，列表在第一条未读上方显示「以下为新消息」（dump 坐标 `[613,2282]`） |
| 已读上报 | 通过（修复），`POST /v1/conversations/{id}/read` 需要 `{"up_to": id}`；之前不带 body 一直 400，未读角标清不掉。现在打开会话后列表角标消失 |
| ntfy 话题注册 | 通过，App 生成 `imbo-bhcyohg2p01q` 并注册成功（日志 `register ntfy topic=imbo-bhc… ok=true`） |
| ntfy 到手机 | 通过，手机装 ntfy 客户端并订阅话题后，服务端发消息 → 通知栏出现 `bob · 标题正文都对了`；群聊 @ 的通知正文是 `bob @了你：…` |
| 账号页「检查连接」 | 通过，点一下显示「连接正常 · 服务端 0.2.17」；失败会显示原因，方便自建用户排障 |
| Tailscale 公网链路 | 通过，手机与电脑同 tailnet，App 指向 `https://desktop-cl5gdeg.tail2dc296.ts.net`；发出消息（id 137）、通知栏收到「走 Tailscale 的通知」、点通知进会话 |
| 点通知进会话 | 通过，ntfy 载荷带 `"click":"imbo://chat/1"`；点通知时系统提示「ntfy 想要打开 Imbo」，允许后直接落到与 bob 的会话 |
| 重装后话题不变 | 通过，清数据重装后话题仍由 `ANDROID_ID` 派生，通知栏照样收到 `bob · 稳定话题的通知` |
| 去掉 Firebase 后回归 | 通过，APK 18.65 → 17.79 MB、release 2.5 → 2.05 MB；装机启动正常，注册话题与通知照旧 |
| 回到最新消息 | 通过，bob 会话上滑后右下角出现圆形按钮，点一下滚回底部，按钮随即消失 |

## 已知差距

- 推送链路已就位，但**没有 Firebase 项目就没有 `google-services.json`**：此时 `BuildConfig.PUSH_ENABLED=false`，不注册令牌、收不到通知；服务端也要配 `IMBO_FCM_*` 才会发。真机推送验收等这两样配好再做。
- 国内网络下 FCM 还有第二道坎：手机连不上 Google，需要代理；即使挂了代理，`firebaseinstallations.googleapis.com` 能通（令牌能拿到、服务端 `push_sent provider="fcm" failed=0`），但 GMS 的推送长连接（`mtalk.google.com:5228`）未必能建立——Clash 日志里看不到 5228 流量时，消息就停在 Google 那侧，通知不会出现。国内长期使用建议接小米推送或自建 ntfy。
- 订阅内购未做（服务端 billing 关闭）。
- @ 提及只做了「插入 @名字 + 加粗显示 + 依赖服务端 mention 通知」；通知那一步受推送通道限制，和上面同一条。
- 多选分享图没有单元测试覆盖（纯 Canvas 绘制），验证靠真机截图；字体用系统等宽，未做中文字体嵌入。
- 会话重命名只对 agent 会话开放（服务端只提供 `/v1/agents/{id}/title`），单聊会话没有可改的名字。
- 新建的群要发出第一条消息后才会出现在对话列表（服务端行为），App 用建群时缓存的群资料撑起聊天页与空态。
- 群信息页的「我的会话」只在账号真有 agent 会话时出现；alice 这类纯 App 账号看不到这一节。
- 收到的远端图片没有本地缓存，`/v1/files/{id}` 需要带 token 下载，目前远端图片退化成文件名 + 大小显示。
- 服务端返回的错误原因目前直接显示英文（如 `carol is already your contact.`），只有登录相关错误做了本地化。
- 像素级视觉对齐只到「结构 + 尺寸 + 颜色」这一层；逐屏与原型的并排比对需要人眼看 `shots/` 与 `imbo-screens/`。
- 1.5 倍字号只验证了能完整渲染，没有验证极端长文本的换行与截断。
