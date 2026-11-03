package dev.imbo.android.data.remote

import dev.imbo.android.data.ContactShare
import dev.imbo.android.data.Account
import dev.imbo.android.data.AgentSession
import dev.imbo.android.data.Attachment
import dev.imbo.android.data.Billing
import dev.imbo.android.data.BlockedAccount
import dev.imbo.android.data.ChangeArea
import dev.imbo.android.data.Contact
import dev.imbo.android.data.ContactRequest
import dev.imbo.android.data.Conversation
import dev.imbo.android.data.Device
import dev.imbo.android.data.GroupDetail
import dev.imbo.android.data.ImboApi
import dev.imbo.android.data.Message
import dev.imbo.android.data.MessagesPage
import dev.imbo.android.data.Profile
import dev.imbo.android.data.SessionInfo
import dev.imbo.android.data.SessionState
import kotlinx.coroutines.flow.Flow
import kotlinx.serialization.builtins.serializer
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.MultipartBody
import okhttp3.RequestBody.Companion.toRequestBody
import javax.inject.Inject
import javax.inject.Singleton

/**
 * 真实接口实现。字段映射见 Mapper；地址规则见 imbo-server/src/routes/messages.rs：
 * 单聊 `to` 是对方账号 ID，群聊是 `#群ID`，自己的会话是 `账号ID/短ID`。
 */
@Singleton
class RemoteImboApi @Inject constructor(
    private val http: ImboHttp,
    private val store: SessionState,
    private val events: EventSocket,
    private val localFiles: LocalFileSource,
    private val pictures: PictureSource,
    private val files: dev.imbo.android.data.AttachmentStore,
) : ImboApi {

    /** 群 id → 群资料：刚建好的群还没会话记录时靠它撑起聊天页。 */
    private val knownGroups = mutableMapOf<Long, dev.imbo.android.data.Profile>()

    /** 群 id → 服务端会话 id：第一条消息发出后才存在。 */
    private val groupConversations = mutableMapOf<Long, Long>()

    override val changes: Flow<ChangeArea> = events.areas

    private val json get() = http.json
    private var cachedConversations: List<Conversation> = emptyList()

    // ------------------------------------------------------------ auth

    override suspend fun contactShare(accountId: String): ContactShare =
        Mapper.contactShare(http.get("/v1/contacts/$accountId/share", ShareResponseDto.serializer()))

    override suspend fun setContactShare(
        accountId: String,
        share: String,
        devices: List<String>,
        sessions: List<String>,
    ): ContactShare {
        val payload = json.encodeToString(
            ShareBodyDto.serializer(),
            ShareBodyDto(share, devices.ifEmpty { null }, sessions.ifEmpty { null }),
        )
        return Mapper.contactShare(
            http.put("/v1/contacts/$accountId/share", payload, ShareResponseDto.serializer()),
        )
    }
    override suspend fun health(): String = http.getUnit("/v1/health")
    override suspend fun requestCode(email: String) {
        http.post(
            "/v1/auth/code",
            json.encodeToString(CodeRequestDto.serializer(), CodeRequestDto(email)),
            CodeResponseDto.serializer(),
        )
    }

    override suspend fun createSession(email: String, code: String, name: String?): SessionInfo {
        val body = AppLoginRequestDto(email = email, code = code, name = name, platform = "android")
        val res = http.post(
            "/v1/app/session",
            json.encodeToString(AppLoginRequestDto.serializer(), body),
            AppLoginResponseDto.serializer(),
        )
        store.token = res.token
        store.accountId = res.account.id
        // 登录响应里没有 access，再取一次 /v1/me 才能拿到试用 / 订阅状态
        val account = me()
        return SessionInfo(account, res.newAccount)
    }

    override suspend fun signOut() {
        runCatching { http.deleteUnit("/v1/app/session") }
        store.clear()
    }

    override suspend fun updateProfile(name: String?, email: String?): Account {
        if (!email.isNullOrBlank()) {
            throw ImboApiException(
                status = 400,
                code = "email_needs_code",
                message = "Changing your email needs a verification code. That flow isn't in the app yet.",
            )
        }
        val body = json.encodeToString(RenameDto.serializer(), RenameDto(name.orEmpty()))
        http.patch("/v1/me", body, Unit.serializer())
        return me()
    }

    override suspend fun me(): Account {
        val res = http.get("/v1/me", MeResponseDto.serializer())
        store.accountId = res.account.id
        return Mapper.account(res.account, res.access).copy(avatarUrl = absoluteAvatar(res.account.avatarUrl))
    }

    override suspend fun uploadAvatar(localUri: String): Account {
        val picture = pictures.read(localUri)
        // 服务端直接读原始图片字节，Content-Type 只能是 image/jpeg 或 image/png
        http.putBytes("/v1/me/avatar", picture.bytes, picture.mime, AvatarResponseDto.serializer())
        // 上传响应只带 avatar_url，再取一次 /v1/me 才是完整账号（含 access / 试用天数）
        return me()
    }

    override suspend fun clearAvatar(): Account {
        http.deleteUnit("/v1/me/avatar")
        return me()
    }

    override suspend fun requestEmailCode(email: String) {
        val body = json.encodeToString(CodeRequestDto.serializer(), CodeRequestDto(email.trim()))
        http.postUnit("/v1/me/email/code", body)
    }

    override suspend fun changeEmail(email: String, code: String): Account {
        val body = json.encodeToString(
            ChangeEmailDto.serializer(),
            ChangeEmailDto(email.trim(), code.trim()),
        )
        // 服务端返回更新后的账号，这里再取一次 /v1/me，省一个只用了 account 的 DTO
        http.post("/v1/me/email", body, AccountResponseDto.serializer())
        return me()
    }

    override suspend fun deleteAccount() {
        // 服务端要求正文里明确写 DELETE，缺了这个会回 400
        val body = json.encodeToString(DeleteAccountDto.serializer(), DeleteAccountDto("DELETE"))
        http.deleteUnit("/v1/account", body)
        store.clear()
    }

    override suspend fun registerPush(token: String, locale: String) {
        val body = json.encodeToString(PushTokenDto.serializer(), PushTokenDto(token, locale))
        http.putUnit("/v1/app/push", body)
    }

    override suspend fun unregisterPush() {
        http.deleteUnit("/v1/app/push")
    }

    override suspend fun setGroupAvatar(groupId: String, localUri: String): String? {
        val picture = pictures.read(localUri)
        val res = http.putBytes(
            "/v1/groups/$groupId/avatar",
            picture.bytes,
            picture.mime,
            AvatarResponseDto.serializer(),
        )
        return absoluteAvatar(res.avatarUrl)
    }

    override suspend fun clearGroupAvatar(groupId: String) {
        http.deleteUnit("/v1/groups/$groupId/avatar")
    }

    /** 服务端没配公开域名时 avatar_url 是相对路径，补上 base 才能交给 Coil 加载。 */
    private fun absoluteAvatar(url: String?): String? = when {
        url.isNullOrBlank() -> null
        url.startsWith("http://") || url.startsWith("https://") -> url
        url.startsWith("/") -> http.base + url
        else -> "${http.base}/$url"
    }

    // ------------------------------------------------------------ conversations

    override suspend fun conversations(): List<Conversation> {
        val res = http.get("/v1/conversations", ConversationsResponseDto.serializer())
        // 服务端没配公开域名时头像是相对路径，补上 base 才能交给 Coil 加载
        cachedConversations = res.conversations.map { dto ->
            Mapper.conversation(dto, store.accountId).let { c ->
                c.copy(
                    peer = c.peer?.copy(avatarUrl = absoluteAvatar(c.peer?.avatarUrl)),
                    group = c.group?.copy(avatarUrl = absoluteAvatar(c.group?.avatarUrl)),
                )
            }
        }
        return cachedConversations
    }

    override suspend fun conversation(id: Long): Conversation? {
        if (id < 0) {
            val res = runCatching { http.get("/v1/agents", AgentsResponseDto.serializer()) }.getOrNull()
                ?: return null
            val match = res.agents.firstOrNull { Mapper.agentConversationId(it.id) == id } ?: return null
            return Mapper.agentConversation(match, store.accountId)
        }
        val all = if (cachedConversations.isEmpty()) conversations() else cachedConversations
        all.firstOrNull { it.id == id }?.let { return it }
        // 刚建的群在服务端还没有会话记录：用建群时缓存的群信息兜底，
        // 界面上就是「这个群还没有聊天记录」，第一条消息发出后服务端才会建会话
        return knownGroups[id]?.let { profile ->
            Conversation(id = id, kind = "group", group = profile, lastMessage = null, pending = 0)
        }
    }

    override suspend fun messages(conversationId: Long, before: Long?, limit: Int): MessagesPage {
        val query = "?limit=$limit" + (before?.let { "&before=$it" } ?: "")
        // 新建的群传进来的是群 id；发过消息后服务端会分配会话 id，这里用映射换成它
        val realId = groupConversations[conversationId] ?: conversationId
        val page = if (conversationId < 0) {
            val agent = agentUuidFor(conversationId)
                ?: throw ImboApiException(404, "not_found", "No such session.")
            http.get("/v1/agents/$agent/messages$query", MessagesPageDto.serializer())
        } else {
            try {
                http.get("/v1/conversations/$realId/messages$query", MessagesPageDto.serializer())
            } catch (e: ImboApiException) {
                // 刚建的群还没有会话记录，服务端回 404，界面按空列表处理
                if (e.status == 404) return MessagesPage(emptyList(), false)
                throw e
            }
        }
        return MessagesPage(
            messages = page.messages.map { dto ->
                val message = Mapper.message(dto, conversationId, store.accountId)
                // 服务端只给文件 id；本地有缓存就补上 file:// 路径，图片才显示得出来
                message.copy(
                    attachments = message.attachments.map { att ->
                        if (att.localUri != null) att else att.copy(localUri = files.find(att.id))
                    },
                )
            },
            hasMore = page.hasMore,
        )
    }

    override suspend fun downloadAttachment(id: String, name: String): String? {
        files.find(id)?.let { return it }
        return runCatching {
            val bytes = http.getBytes("/v1/files/$id")
            files.save(id, bytes, name)
        }.getOrNull()
    }

    /**
     * 上传附件。服务端 /v1/files 读的是原始请求体，不是 multipart：
     * Imbo-File-Name 放百分号编码的文件名，Imbo-File-Sha256 放内容哈希（64 位小写十六进制），
     * 返回 {file, quota}，这里取 file.id 在发送时放进 files。
     */
    override suspend fun uploadAttachment(localUri: String, name: String, mimeType: String): Attachment {
        val bytes = localFiles.read(localUri)
        val digest = java.security.MessageDigest.getInstance("SHA-256").digest(bytes)
        val sha256 = digest.joinToString("") { "%02x".format(it) }
        val res = http.postRawFile(
            path = "/v1/files",
            bytes = bytes,
            fileName = name.ifBlank { "attachment" },
            sha256Hex = sha256,
            contentType = mimeType,
            serializer = UploadResponseDto.serializer(),
        )
        // 上传后把原字节在本机留一份，重新从服务端拉消息时还能显示图片
        val cached = runCatching { files.save(res.file.id, bytes, res.file.name) }.getOrNull()
        return Mapper.attachment(res.file, localUri = cached ?: localUri, mimeType = mimeType)
    }

    override suspend fun sendMessage(
        conversationId: Long,
        body: String,
        clientId: String,
        attachments: List<Attachment>,
    ): Message {
        val now = System.currentTimeMillis()
        val fileIds = attachments.map { it.id }
        if (conversationId < 0) {
            val agent = agentUuidFor(conversationId)
                ?: throw ImboApiException(404, "not_found", "No such session.")
            val payload = json.encodeToString(
                SendToAgentBodyDto.serializer(),
                SendToAgentBodyDto(body, clientId, fileIds),
            )
            val res = http.post("/v1/agents/$agent/messages", payload, MessageResponseDto.serializer())
            return Mapper.message(res.message, conversationId, store.accountId)
                .copy(clientId = clientId, attachments = attachments)
        }

        val conversation = conversation(conversationId)
            ?: throw ImboApiException(404, "not_found", "No such conversation.")
        val address = if (conversation.isGroup) {
            "#${conversation.group?.id}"
        } else {
            conversation.peer?.id.orEmpty()
        }
        val payload = json.encodeToString(
            SendBodyDto.serializer(),
            SendBodyDto(to = address, body = body, clientMsgId = clientId, files = fileIds),
        )
        val res = http.post("/v1/messages", payload, SendResponseDto.serializer())
        // 群里第一条消息会创建会话，把群 id 映射到真实会话 id，后续拉消息才找得到
        if (conversation.isGroup) {
            res.conversation?.id?.let { groupConversations[conversationId] = it }
        }
        return Message(
            id = res.messageId,
            conversationId = conversationId,
            body = body,
            senderName = "me",
            mine = true,
            createdAt = now,
            clientId = clientId,
            attachments = attachments,
        )
    }

    override suspend fun markRead(agentId: String) {
        http.postUnit("/v1/agents/$agentId/read")
    }

    /** 服务端要求带上读到哪一条：`up_to` 是消息 id，缺了会 400。 */
    override suspend fun markConversationRead(conversationId: Long, upTo: Long) {
        http.postUnit("/v1/conversations/$conversationId/read", "{\"up_to\":$upTo}")
    }

    // ------------------------------------------------------------ agents

    override suspend fun agents(): List<AgentSession> {
        val res = http.get("/v1/agents", AgentsResponseDto.serializer())
        return res.agents.map { Mapper.agentSession(it) }
    }

    private suspend fun agentUuidFor(conversationId: Long): String? {
        val res = runCatching { http.get("/v1/agents", AgentsResponseDto.serializer()) }.getOrNull()
            ?: return null
        return res.agents.firstOrNull { Mapper.agentConversationId(it.id) == conversationId }?.id
    }

    // ------------------------------------------------------------ contacts

    override suspend fun contacts(): List<Contact> {
        val res = http.get("/v1/contacts", ContactsResponseDto.serializer())
        return res.contacts.filterNot { it.blocked }.map { Mapper.contact(it) }
    }

    override suspend fun requests(): List<ContactRequest> {
        val res = http.get("/v1/requests", RequestsResponseDto.serializer())
        return res.requests.filter { it.status == "pending" }.map { Mapper.request(it) }
    }

    override suspend fun acceptRequest(id: String) {
        http.postUnit("/v1/requests/$id/accept")
    }

    override suspend fun declineRequest(id: String) {
        http.postUnit("/v1/requests/$id/decline")
    }

    override suspend fun addContact(accountId: String) {
        val numeric = accountId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Account IDs are numbers.")
        val payload = json.encodeToString(
            ContactRequestDto.serializer(),
            ContactRequestDto(accountId = numeric),
        )
        http.postUnit("/v1/contacts/requests", payload)
    }

    /** 服务端两种定位方式二选一：正文里给 account_id 或 email（API-008）。 */
    override suspend fun addContactByEmail(email: String) {
        val payload = json.encodeToString(
            ContactRequestDto.serializer(),
            ContactRequestDto(email = email.trim()),
        )
        http.postUnit("/v1/contacts/requests", payload)
    }

    override suspend fun removeContact(accountId: String) {
        http.deleteUnit("/v1/contacts/$accountId")
    }

    override suspend fun renameAgent(agentId: String, title: String) {
        val payload = json.encodeToString(TitleBodyDto.serializer(), TitleBodyDto(title.trim()))
        http.put("/v1/agents/$agentId/title", payload, AgentResponseDto.serializer())
    }

    // ------------------------------------------------------------ groups

    override suspend fun createGroup(name: String, memberIds: List<String>, sessions: List<String>): Conversation {
        val payload = json.encodeToString(CreateGroupBodyDto.serializer(), CreateGroupBodyDto(name, sessions.ifEmpty { null }))
        val res = http.post("/v1/groups", payload, GroupResponseDto.serializer())
        // 服务端的建群接口只收群名，成员要逐个邀请
        memberIds.mapNotNull { it.toIntOrNull() }.forEach { accountId ->
            runCatching {
                val invite = json.encodeToString(InviteBodyDto.serializer(), InviteBodyDto(accountId))
                http.postUnit("/v1/groups/${res.group.id}/invites", invite)
            }
        }
        val profile = Profile(
            id = res.group.id.toString(),
            name = res.group.name,
            // 邀请只是发请求，对方接受后才算成员，这里按服务端真实人数显示
            subtitle = "${res.group.members.size} members",
        )
        // 建群时服务端还没有会话记录，先按群 id 缓存资料，聊天页靠它显示标题与空态
        knownGroups[res.group.id.toLong()] = profile
        return Conversation(
            id = res.group.id.toLong(),
            kind = "group",
            group = profile,
            lastMessage = null,
            pending = 0,
        )
    }

    /**
     * 群管理对齐 imbo-server/src/routes/groups.rs：列表、改名、邀请、移除、退群、解散。
     * 服务端没有 GET /v1/groups/{id}，详情从列表接口的完整 GroupView 里按 id 取。
     * 这里只按 api.rs 的字段和路由表实现，没有可联调的服务端，未在真机上跑过。
     */
    override suspend fun groupDetail(groupId: String): GroupDetail {
        val id = groupId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Group IDs are numbers.")
        val res = http.get("/v1/groups", GroupsResponseDto.serializer())
        val dto = res.groups.firstOrNull { it.id == id }
            ?: throw ImboApiException(404, "not_found", "No such group.")
        return Mapper.groupDetail(dto, store.accountId)
            .copy(avatarUrl = absoluteAvatar(dto.avatarUrl))
    }

    override suspend fun renameGroup(groupId: String, name: String) {
        val id = groupId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Group IDs are numbers.")
        val payload = json.encodeToString(RenameDto.serializer(), RenameDto(name))
        http.patch("/v1/groups/$id", payload, GroupResponseDto.serializer())
    }

    override suspend fun inviteToGroup(groupId: String, accountId: String) {
        val id = groupId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Group IDs are numbers.")
        val account = accountId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Account IDs are numbers.")
        val payload = json.encodeToString(InviteBodyDto.serializer(), InviteBodyDto(account))
        http.postUnit("/v1/groups/$id/invites", payload)
    }

    /** 只有我的会话能被选：服务端会校验这些 id 属于当前账号。 */
    override suspend fun setGroupSessions(groupId: String, agentIds: List<String>) {
        val id = groupId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Group IDs are numbers.")
        val payload = json.encodeToString(GroupSessionsDto.serializer(), GroupSessionsDto(agentIds))
        http.put("/v1/groups/$id/sessions", payload, MySessionsDto.serializer())
    }

    override suspend fun removeFromGroup(groupId: String, accountId: String) {
        val id = groupId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Group IDs are numbers.")
        val account = accountId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Account IDs are numbers.")
        http.deleteUnit("/v1/groups/$id/members/$account")
    }

    override suspend fun leaveGroup(groupId: String) {
        val id = groupId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Group IDs are numbers.")
        http.deleteUnit("/v1/groups/$id/members/me")
    }

    override suspend fun dissolveGroup(groupId: String) {
        val id = groupId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Group IDs are numbers.")
        http.deleteUnit("/v1/groups/$id")
    }

    override suspend fun transferGroupOwner(groupId: String, accountId: String) {
        val id = groupId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Group IDs are numbers.")
        val target = accountId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Account IDs are numbers.")
        val payload = json.encodeToString(OwnerBodyDto.serializer(), OwnerBodyDto(target))
        http.put("/v1/groups/$id/owner", payload, GroupResponseDto.serializer())
    }

    // ------------------------------------------------------------ devices

    override suspend fun devices(): List<Device> {
        val res = http.get("/v1/devices", DevicesResponseDto.serializer())
        return res.devices.map { Mapper.device(it) }
    }

    override suspend fun revokeDevice(id: String) {
        http.deleteUnit("/v1/devices/$id")
    }

    // ------------------------------------------------------------ safety

    override suspend fun blocked(): List<BlockedAccount> {
        val res = http.get("/v1/blocks", BlocksResponseDto.serializer())
        return res.blocks.map { Mapper.block(it) }
    }

    override suspend fun block(accountId: String) {
        val numeric = accountId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Sessions can't be blocked, the account can.")
        val payload = json.encodeToString(BlockBodyDto.serializer(), BlockBodyDto(numeric))
        http.postUnit("/v1/blocks", payload)
    }

    override suspend fun unblock(accountId: String) {
        http.deleteUnit("/v1/blocks/$accountId")
    }

    override suspend fun report(targetId: String, reason: String) {
        val numeric = targetId.toIntOrNull()
            ?: throw ImboApiException(400, "invalid", "Reports need an account.")
        val mapped = when (reason.lowercase()) {
            "spam" -> "spam"
            "harassment" -> "harassment"
            else -> "other"
        }
        val payload = json.encodeToString(
            ReportBodyDto.serializer(),
            ReportBodyDto(accountId = numeric, reason = mapped),
        )
        http.postUnit("/v1/reports", payload)
    }

    // ------------------------------------------------------------ billing

    override suspend fun billing(): Billing {
        val res = http.get("/v1/billing", BillingResponseDto.serializer())
        return Mapper.billing(res)
    }
}
