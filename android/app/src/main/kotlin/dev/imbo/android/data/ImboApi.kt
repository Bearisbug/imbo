package dev.imbo.android.data

import kotlinx.coroutines.flow.Flow

/**
 * 唯一的网络契约。mock 与真实实现都实现它，切换只发生在 Hilt 注入点。
 */
interface ImboApi {
    /** POST /v1/auth/code */
    /** 某个联系人的会话共享设置：all / chosen / none。 */
    suspend fun contactShare(accountId: String): ContactShare

    /** 改共享设置；chosen 时用 devices / sessions 指定可见范围。 */
    suspend fun setContactShare(
        accountId: String,
        share: String,
        devices: List<String> = emptyList(),
        sessions: List<String> = emptyList(),
    ): ContactShare
    /** 自检用：拿服务端的 /v1/health 原文，失败抛异常。 */
    suspend fun health(): String
    suspend fun requestCode(email: String)

    /** POST /v1/app/session，platform 固定 android；name 非空表示注册。 */
    suspend fun createSession(email: String, code: String, name: String? = null): SessionInfo

    /** DELETE /v1/app/session */
    suspend fun signOut()

    /** PATCH /v1/me */
    suspend fun updateProfile(name: String? = null, email: String? = null): Account

    /** PUT /v1/me/avatar：localUri 是相册返回的 content:// 或 file:// 地址。 */
    suspend fun uploadAvatar(localUri: String): Account

    /** DELETE /v1/me/avatar：删除后回到名字首字母头像。 */
    suspend fun clearAvatar(): Account

    /** POST /v1/me/email/code：验证码只发给新邮箱，旧邮箱收不到。 */
    suspend fun requestEmailCode(email: String)

    /** POST /v1/me/email：验证码正确就换掉登录邮箱，返回更新后的账号。 */
    suspend fun changeEmail(email: String, code: String): Account

    /** DELETE /v1/account：注销账号，服务端删完数据后本地登录态也要清掉。 */
    suspend fun deleteAccount()

    /** PUT /v1/app/push：让这台手机从现在开始收推送（Expo 或 FCM 令牌服务端都收）。 */
    suspend fun registerPush(token: String, locale: String)

    /** DELETE /v1/app/push：退出登录时把推送停掉。 */
    suspend fun unregisterPush()

    /** PUT /v1/groups/{id}/avatar：群主换群头像，返回新的图片地址。 */
    suspend fun setGroupAvatar(groupId: String, localUri: String): String?

    /** DELETE /v1/groups/{id}/avatar：群主清掉群头像，回到首字母。 */
    suspend fun clearGroupAvatar(groupId: String)

    suspend fun me(): Account
    suspend fun conversations(): List<Conversation>
    suspend fun conversation(id: Long): Conversation?
    suspend fun messages(conversationId: Long, before: Long? = null, limit: Int = 50): MessagesPage
    suspend fun sendMessage(
        conversationId: Long,
        body: String,
        clientId: String,
        attachments: List<Attachment> = emptyList(),
    ): Message

    /**
     * 上传一个本机文件。mock 直接回填原 localUri 不真上传；
     * 真实实现 POST /v1/files，返回服务端文件 id，发送时放进消息的 files。
     */
    suspend fun uploadAttachment(localUri: String, name: String, mimeType: String): Attachment

    /**
     * 下载消息里的附件到本机，返回 file:// 路径；已经下载过直接返回。
     * 拿不到（例如服务端没存）时返回 null，界面退回文件名展示。
     */
    suspend fun downloadAttachment(id: String, name: String): String?

    /** POST /v1/conversations/{id}/read：打开单聊或群聊时把未读清零。 */
    suspend fun markConversationRead(conversationId: Long, upTo: Long)

    suspend fun markRead(agentId: String)
    suspend fun agents(): List<AgentSession>
    suspend fun contacts(): List<Contact>
    suspend fun requests(): List<ContactRequest>
    suspend fun acceptRequest(id: String)
    suspend fun declineRequest(id: String)
    suspend fun addContact(accountId: String)

    /** 按邮箱加联系人：服务端会用邮箱找到账号，再走同一条请求流程。 */
    suspend fun addContactByEmail(email: String)

    /** DELETE /v1/contacts/{account_id}：只解除联系人关系，历史消息保留。 */
    suspend fun removeContact(accountId: String)

    /** PUT /v1/agents/{id}/title：给自己的一台会话改名。 */
    suspend fun renameAgent(agentId: String, title: String)

    suspend fun createGroup(name: String, memberIds: List<String>): Conversation

    /** PUT /v1/groups/{id}/owner：群主把群交给另一位成员。 */
    suspend fun transferGroupOwner(groupId: String, accountId: String)

    /** GET /v1/groups（服务端没有按 id 取群的端点，实现里从列表挑一条）。 */
    suspend fun groupDetail(groupId: String): GroupDetail

    /** PATCH /v1/groups/{id} */
    suspend fun renameGroup(groupId: String, name: String)

    /** POST /v1/groups/{id}/invites */
    suspend fun inviteToGroup(groupId: String, accountId: String)

    /** PUT /v1/groups/{id}/sessions：选自己哪些会话接收这个群的消息。 */
    suspend fun setGroupSessions(groupId: String, agentIds: List<String>)

    /** DELETE /v1/groups/{id}/members/{account_id} */
    suspend fun removeFromGroup(groupId: String, accountId: String)

    /** DELETE /v1/groups/{id}/members/me */
    suspend fun leaveGroup(groupId: String)

    /** DELETE /v1/groups/{id} */
    suspend fun dissolveGroup(groupId: String)

    suspend fun devices(): List<Device>
    suspend fun revokeDevice(id: String)
    suspend fun blocked(): List<BlockedAccount>
    suspend fun block(accountId: String)
    suspend fun unblock(accountId: String)
    suspend fun billing(): Billing

    /**
     * 举报会话或账号。服务端还没有 /v1/reports，这里先把入口和状态做出来，
     * 真实实现接上后只需换掉这个方法的内部。
     */
    suspend fun report(targetId: String, reason: String)

    /** 服务端 /v1/events 只推变更通知，收到后按 area 重新拉取。 */
    val changes: Flow<ChangeArea>
}

enum class ChangeArea { All, Conversations, Messages, Agents, Contacts, Requests, Billing }
