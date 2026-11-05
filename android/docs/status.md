# 当前状态：功能 / 验证 / 阻塞

更新时间：2026-11-03。判定标准：**已实现**＝代码在仓库里且有构建产物；**已验证**＝在真机或真实服务端跑通过，留有截图或日志；**未验证**＝代码在但没跑过；**阻塞**＝外部条件不具备。

## 客户端功能

| 项 | 状态 | 证据 |
| --- | --- | --- |
| 登录 / 注册 / 验证码重发倒计时 | 已验证 | 真机登录 alice；倒计时 60→0 后恢复可点 |
| 记住上次邮箱 | 已验证 | 退出登录后邮箱已预填 |
| 服务器地址可配（release 强制 https） | 已验证 | 登录页与账号页均可改；改完强制重登 |
| 账号页「检查连接」 | 已验证 | 真机显示「连接正常 · 服务端 0.2.17」；有单测覆盖成功/失败 |
| 联系人：ID/邮箱加人、接受/拒绝、删除 | 已验证 | 邮箱 carol 请求出现在 `/v1/requests`，接受后成为联系人 |
| 单聊 / 群聊 / 建群 / 邀请 | 已验证 | 建了 mention-test，bob 接受后成员变 2 |
| 群管理：改名、移除、转让、群头像、选会话、退群/解散 | 已验证 | 转让后群主变 bob；群头像更新；会话勾选生效 |
| 附件：相册 / 文件 / 拍照 | 已验证 | 拍照全链路：相机→确认→服务端 `files[0]` 3.7 MB |
| 图片全屏查看 | 已验证 | 黑底等比缩放，点任意位置关闭 |
| @ 提及 | 已验证 | 输入 @ 弹出成员列表（含刚接受邀请的成员），发送后服务端正文为 `@bob` |
| 转发消息 | 已验证 | 群消息转发到 bob，服务端出现同正文 |
| 多选分享图 | 已验证 | 分享面板出现 `imbo-….png` 预览 |
| 会话搜索 | 已验证 | 搜 `bob` 只剩 bob；搜 `zzz` 显示空态 |
| 未读分隔线 | 已验证 | UI dump 在第一条未读上方出现「以下为新消息」 |
| 已读上报 | 已验证（修复） | 服务端要求 `up_to`，补上后角标清零 |
| 发送失败重试 / 网络错误中文化 | 已验证 | 停服务端→失败提示→恢复后点气泡重发成功 |
| 回到最新消息 | 已验证 | 上滑出现圆钮，点后回底 |
| 深链 `imbo://chat/<id>` | 已验证 | 冷启动直接落到 bob 会话 |
| 换邮箱 / 删账号 / 退出登录 | 已验证 | 两步验证码、删账号回登录页 |
| 会话列表「在线 / 最后在线」 | 已验证（mock） | mock 数据下显示「Codex · Studio · 最后在线 20:57」 |

## 推送（ntfy）

| 项 | 状态 | 证据 |
| --- | --- | --- |
| 服务端发到 ntfy | 已验证 | `push event="push_sent" provider="ntfy" message_id=99 tokens=1` |
| 客户端收到并弹通知 | 已验证 | 通知栏「bob · 标题正文都对了」 |
| 群 @ 的通知文案 | 已验证 | 「bob @了你：@alice 你看下这个」 |
| 点通知进会话 | 已验证 | 系统询问「ntfy 想要打开 Imbo」→ 允许后落到会话 |
| 话题重装不变 | 已验证 + 单测 | 由 `ANDROID_ID` 派生，重装后仍是同一话题并收到通知；派生规则有 4 条单测（稳定性、字符集、不同设备不同话题、缺失 ID 兜底） |
| Firebase / FCM | 已移除 | debug 18.65→17.91 MB，release 2.5→2.05 MB，启动无 Google 依赖 |

## 服务端与部署

| 项 | 状态 | 证据 |
| --- | --- | --- |
| ntfy 补丁（Rust） | 已验证 | `cargo test -p imbo-server --bins` 41 通过；发布走根路径 JSON |
| Docker compose（imbo + ntfy） | 已验证 | 本机冒烟：两个 `/v1/health` 均 ok |
| 一键出包脚本 | 已验证 | `scripts/release.ps1` 产出 APK+AAB 并打印 sha256 |
| 部署自检脚本 | 已验证 | `scripts/check-deployment.ps1` 三项全过，手机收到自检通知；服务端 `/v1/health` 现在会报 `"push":{"expo":…,"fcm":…,"ntfy":…}`，脚本会断言 ntfy 已配置 |
| Tailscale 公网访问 | 已验证 | 本机已装 Tailscale（`imbo.<tailnet>.ts.net`），`tailscale serve` 暴露 imbo 与 ntfy(:8443)；手机与电脑同 tailnet |
| 手机走公网收发 | 已验证 | 手机 Tailscale 连上后，App 服务器改为 `https://imbo.<tailnet>.ts.net`：消息发出（id 137）、通知栏收到「走 Tailscale 的通知」、点通知进会话 |
| 正式地址 release 包 | 已验证（构建） | `release.ps1 -BaseUrl https://imbo.<tailnet>.ts.net -NtfyUrl …:8443`，APK 2.05 MB、sha256 `e0639489…`，dex 内已核对地址、签名证书 `e2eb1a90…` |
| CI tag | **待授权** | 仓库改动未提交，推 tag 前需要先提交并推送；属发布动作，等用户确认 |

## 当前阻塞（一条）

MIUI 关掉了「USB 安装」，`adb install` 一律 `INSTALL_FAILED_USER_RESTRICTED`；手机浏览器下载后弹出的 MIUI 安装器确认页不响应 adb 注入点击。手机上现在**没有装 Imbo**，安装器窗口停在「继续安装」。

两条解除方式：手机上直接点「继续安装」，或打开 设置 → 更多设置 → 开发者选项 → USB 安装。

## 质量数据

- App 单测 **103 个全绿**：MockImboApiTest 31、RemoteImboApiTest 26、ViewModelTest 32、ServerConfigStoreTest 5、ErrorTextTest 5、NtfyStoreTest 4
- 服务端测试 **41 个全绿**
- `dist/`：debug 17.91 MB、mock 18.09 MB、release APK 2.05 MB、AAB 4.34 MB
## 最终构建（本轮）

- 源码：main @ 8db2276（含联系人备注、同名会话区分、跨账号多 agent 验证）
- 产物：APK 2.06 MB，sha256 1f4798b8fba2f1f89d7f952faa5cb8047d18d8d70d64c5ad4441ff9d89d916d1
- 产物：AAB 4.36 MB，sha256 04f624dbb1b68b25fa9f733872c2dc02c30dd906590ce77b61cfc4966c5bebaa
- 校验：证书 e2eb1a90…，dex 内含 https://desktop-cl5gdeg.tail2dc296.ts.net，versionCode 2 / versionName 1.0.0
- 说明：该构建早于联系人备注与同名会话区分的代码，安装前需重新出包
## 最终构建（含最新修复）

- 源码：main @ 3884545（联系人备注、同名会话聊天内区分）
- APK 2.06 MB，sha256 426e34efeb99a6607d700330fb3cb51561bcf6edb1a397618ee13473e661ac15
- AAB 4.36 MB，sha256 d887c78ba84b1b0a42cd09acda37388c4333fbc07d1b30bebc3d455f3d15e23a
- 校验：证书 e2eb1a90…，dex 内含 https://desktop-cl5gdeg.tail2dc296.ts.net，versionCode 2 / versionName 1.0.0

## dist 与源码一致性

用 aapt2 检查 dist/imbo-release-signed.apk 的资源字符串，确认包含本轮新增的文案：备注（可选）、等待会话领取、对方能看到的会话（%1 / %2）。说明 dist 里的包是当前 main 构建的，不是旧产物。

## 正式包装机冒烟（已完成）

- 卸载 debug 包后安装 \dist/imbo-release-signed.apk\（sha256 426e34…），首次启动完成登录（alice）并授予通知权限
- 发出的消息 #158 \elease-smoke\ 在服务端可见
- bob 发的 #159 触发 ntfy 通知，通知栏显示「💬 bob｜release push check」
- 冷启动深链 \imbo://chat/1\ 直接进入 bob 会话；\imbo://chat/148\ 进入 cross-account 群，消息标签显示 \worker · ec965c\`r
