package dev.imbo.android.data.mock

import dev.imbo.android.data.ContactShare
import dev.imbo.android.data.SharedSession
import dev.imbo.android.data.Access
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
import dev.imbo.android.data.ForceError
import dev.imbo.android.data.Group
import dev.imbo.android.data.GroupDetail
import dev.imbo.android.data.ImboApi
import dev.imbo.android.data.MockConfig
import dev.imbo.android.data.LastMessage
import dev.imbo.android.data.Message
import dev.imbo.android.data.MessagesPage
import dev.imbo.android.data.Profile
import dev.imbo.android.data.SessionInfo
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.launch

/**
 * mock 通道：同一份契约的本地实现，用来在不依赖服务端的情况下验收界面与交互。
 * 数据内容对齐原型里那套（alice、bob / carol / dave、三个 agent 会话）。
 */
class MockImboApi(
    val config: MutableStateFlow<MockConfig> = MutableStateFlow(MockConfig()),
) : ImboApi {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val changeFlow = MutableSharedFlow<ChangeArea>(extraBufferCapacity = 16)
    override val changes: Flow<ChangeArea> = changeFlow.asSharedFlow()

    private val now = System.currentTimeMillis()
    private fun minutesAgo(m: Int) = now - m * 60_000L
    private fun hoursAgo(h: Int) = now - h * 3_600_000L
    private fun daysAgo(d: Int) = now - d * 86_400_000L

    private var me = Account(
        id = "acc-alice",
        name = "alice",
        email = "alice@example.com",
        accountId = "81234567",
        trialDaysLeft = 20,
        access = Access.Trial(20),
    )

    private val bob = Profile("acc-bob", "bob", "orders-api", online = true)
    private val carol = Profile("acc-carol", "carol", "docs-site", online = false)
    private val dave = Profile("acc-dave", "dave", "Web", online = false)
    private val groupProfile = Profile("grp-release", "v2.3 release", "4 members")
    private val checkout = Profile("agent-checkout", "checkout", "Claude Code", online = true, agent = true)

    private val messageStore = mutableMapOf<Long, MutableList<Message>>()

    private val chats: MutableList<Conversation> = mutableListOf(
        Conversation(
            id = 1L, kind = "direct", peer = bob,
            lastMessage = LastMessage("Migration is live: /v2/orders takes customer_id; user_id is gone.", "bob", hoursAgo(1), false),
            pending = 2, agentId = "agent-orders",
        ),
        Conversation(
            id = 2L, kind = "group", group = groupProfile,
            lastMessage = LastMessage("Changelog is up. Posting it to the group now.", "carol", hoursAgo(2), false),
            pending = 3,
        ),
        Conversation(
            id = 3L, kind = "direct", peer = carol,
            lastMessage = LastMessage("Spec v2 attached. Diff it against /users.", "carol", daysAgo(2), false),
            pending = 0,
        ),
        Conversation(
            id = 4L, kind = "direct", peer = dave,
            lastMessage = LastMessage("Thanks, that unblocks me.", "dave", daysAgo(4), false),
            pending = 0,
        ),
    )

    /** agent 会话不进对话列表，只在「会话」里出现；但仍然能按 id 打开聊天页。 */
    private val sessionChats: List<Conversation> = listOf(
        Conversation(
            id = 5L, kind = "direct", peer = checkout,
            lastMessage = LastMessage("Ship it.", "checkout", minutesAgo(9), false),
            pending = 1, agentId = "agent-checkout",
        ),
    )

    // 一个有电脑名的在线会话 + 一个离线但记得最后在线时间的会话，用来验会话列表那两行信息
    private val sessions = mutableListOf(
        AgentSession(
            "agent-checkout", "checkout", "Claude Code", "idle", 1,
            deviceName = "MacBook", online = true, wakeable = true,
        ),
        AgentSession(
            "agent-docs", "docs-site", "Codex", "working", 1,
            deviceName = "Studio", online = false, lastOnlineAt = hoursAgo(3),
        ),
    )

    private val contactList = mutableListOf(
        Contact(bob.id, bob.name, bob.subtitle, online = true),
        Contact(carol.id, carol.name, carol.subtitle),
        Contact(dave.id, dave.name, dave.subtitle),
        Contact(groupProfile.id, groupProfile.name, "4 members"),
    )

    /**
     * 群成员与群主。改名、移除、邀请都改这一份数据，群信息页和对话列表重新拉取即可看到。
     * 现有 v2.3 release 群固定为 alice + bob + carol + dave，与对话里的「4 members」一致。
     */
    private val groupMembers = mutableMapOf<String, MutableList<Contact>>(
        groupProfile.id to mutableListOf(
            Contact(me.id, me.name),
            Contact(bob.id, bob.name, bob.subtitle, online = true),
            Contact(carol.id, carol.name, carol.subtitle),
            Contact(dave.id, dave.name, dave.subtitle),
        ),
    )
    private val groupOwners = mutableMapOf<String, String>(groupProfile.id to me.id)

    private val requestList = mutableListOf(
        ContactRequest("req-1", "erin", "api team"),
    )

    private val deviceList = mutableListOf(
        Device("dev-mac", "alice's MacBook Pro", "darwin", lastSeenMinutes = 1, thisDevice = false),
        Device("dev-studio", "alice's Studio", "Linux", lastSeenMinutes = 60 * 24 * 3),
    )

    private val blockedList = mutableListOf<BlockedAccount>()

    init {
        messageStore[1L] = mutableListOf(
            Message(1, 1, "What replaced user_id?", "bob", false, hoursAgo(3)),
            Message(2, 1, "customer_id. It's on every /v2/orders row.", "alice", true, hoursAgo(2)),
            Message(3, 1, "Migration is live: /v2/orders takes customer_id; user_id is gone.", "bob", false, hoursAgo(1)),
        )
        messageStore[2L] = mutableListOf(
            Message(10, 2, "Spec v2 attached. Diff it against /users.", "carol", false, hoursAgo(5)),
            Message(11, 2, "Looks right. Ship the migration note with it.", "alice", true, hoursAgo(4)),
            Message(12, 2, "Changelog is up. Posting it to the group now.", "carol", false, hoursAgo(2)),
        )
        messageStore[3L] = mutableListOf(
            Message(
                id = 20,
                conversationId = 3,
                body = "Spec v2 attached. Diff it against /users.",
                senderName = "carol",
                mine = false,
                createdAt = daysAgo(2),
                attachments = listOf(
                    Attachment("file-spec-v2", "spec-v2.pdf", "application/pdf", sizeBytes = 48_200),
                ),
            ),
        )
        messageStore[4L] = mutableListOf(
            Message(30, 4, "Thanks, that unblocks me.", "dave", false, daysAgo(4)),
        )
        messageStore[5L] = mutableListOf(
            Message(40, 5, "Tests pass on the payment retry path.", "checkout", false, minutesAgo(20)),
            Message(41, 5, "Ship it.", "alice", true, minutesAgo(15)),
            Message(42, 5, "Ship it.", "checkout", false, minutesAgo(9)),
        )
    }

    private suspend fun <T> gate(block: suspend () -> T): T {
        val cfg = config.value
        if (cfg.latencyMs > 0) delay(cfg.latencyMs)
        when (cfg.forceError) {
            ForceError.Unauthorized -> throw MockException("401 Unauthorized")
            ForceError.ServerError -> throw MockException("500 Internal Server Error")
            ForceError.Timeout -> throw MockException("timeout")
            null -> Unit
        }
        return block()
    }

    private fun dropIfEmpty() = config.value.emptyData

    override suspend fun me(): Account = gate { me }

    private val contactShareModes = mutableMapOf<String, String>()

    override suspend fun contactShare(accountId: String): ContactShare = gate {
        ContactShare(
            share = contactShareModes[accountId] ?: "all",
            sessions = sessions.map { SharedSession(it.id, it.id.take(6), it.title, it.tool) },
        )
    }

    override suspend fun setContactShare(
        accountId: String,
        share: String,
        devices: List<String>,
        sessions: List<String>,
    ): ContactShare = gate {
        contactShareModes[accountId] = share
        ContactShare(
            share = share,
            sessions = this.sessions
                .filter { sessions.isEmpty() || it.id in sessions }
                .map { SharedSession(it.id, it.id.take(6), it.title, it.tool) },
        )
    }
    // 走 gate，才会受 forceError / latency 影响，测试里能造出失败
    override suspend fun health(): String = gate { """{"status":"ok","name":"imbo-server","version":"mock","db":"ok"}""" }
    override suspend fun requestCode(email: String) = gate {
        if (!email.contains("@") || email.endsWith("@")) {
            throw MockException("Enter a valid email address")
        }
    }

    /**
     * 真实服务端校验验证码后签发 token。这里用固定规则模拟：
     * 6 位数字通过，`111111` 视为验证码错误，其余非 6 位直接拒绝。
     */
    override suspend fun createSession(email: String, code: String, name: String?): SessionInfo = gate {
        if (!email.contains("@")) throw MockException("Enter a valid email address")
        if (!code.all { it.isDigit() } || code.length != 6) throw MockException("That code doesn't look right")
        if (code == "111111") throw MockException("That code is wrong or expired")
        val newAccount = name != null
        me = if (name != null) me.copy(name = name, email = email) else me.copy(email = email)
        SessionInfo(me, newAccount)
    }

    override suspend fun signOut() = gate { Unit }

    override suspend fun updateProfile(name: String?, email: String?): Account = gate {
        me = me.copy(name = name?.takeIf { it.isNotBlank() } ?: me.name, email = email ?: me.email)
        changeFlow.emit(ChangeArea.All)
        me
    }

    /** 头像只存在内存里，进程重启后回到无头像，和真实上传一样以返回的 Account 为准。 */
    override suspend fun uploadAvatar(localUri: String): Account = gate {
        if (localUri.isBlank()) throw MockException("Couldn't read that picture.")
        me = me.copy(avatarUrl = localUri)
        changeFlow.emit(ChangeArea.All)
        me
    }

    override suspend fun clearAvatar(): Account = gate {
        me = me.copy(avatarUrl = null)
        changeFlow.emit(ChangeArea.All)
        me
    }

    /** 换邮箱在 mock 里分两步：先发码（直接当成功），再拿任意 6 位数字换掉。 */
    private var pendingEmail: String? = null

    override suspend fun requestEmailCode(email: String) = gate {
        val target = email.trim()
        if (!target.contains('@') || target.length < 5) throw MockException("That email address doesn't look right.")
        if (target.equals(me.email, ignoreCase = true)) throw MockException("That's already your email.")
        pendingEmail = target
    }

    override suspend fun changeEmail(email: String, code: String): Account = gate {
        val target = email.trim()
        if (pendingEmail == null || !target.equals(pendingEmail, ignoreCase = true)) {
            throw MockException("Request a code for this email first.")
        }
        if (code.trim().length != 6 || code.trim().any { !it.isDigit() }) {
            throw MockException("That code isn't right.")
        }
        pendingEmail = null
        me = me.copy(email = target)
        changeFlow.emit(ChangeArea.All)
        me
    }

    override suspend fun deleteAccount() = gate {
        changeFlow.emit(ChangeArea.All)
        Unit
    }

    /** 群头像 mock 只记本地地址，和真实上传一样以返回值给界面用。 */
    private val groupAvatars = mutableMapOf<String, String>()

    override suspend fun setGroupAvatar(groupId: String, localUri: String): String? = gate {
        requireGroup(groupId)
        if (groupOwners[groupId] != me.id) throw MockException("Only the owner can change this group's picture.")
        if (localUri.isBlank()) throw MockException("Couldn't read that picture.")
        groupAvatars[groupId] = localUri
        changeFlow.emit(ChangeArea.Conversations)
        localUri
    }

    override suspend fun clearGroupAvatar(groupId: String) = gate {
        requireGroup(groupId)
        if (groupOwners[groupId] != me.id) throw MockException("Only the owner can change this group's picture.")
        groupAvatars.remove(groupId)
        changeFlow.emit(ChangeArea.Conversations)
    }

    override suspend fun conversations(): List<Conversation> = gate {
        if (dropIfEmpty()) emptyList() else chats
            .filterNot { blockedList.any { b -> b.id == it.peer?.id } }
            .map { it.withGroupAvatar() }
    }

    override suspend fun conversation(id: Long): Conversation? = gate {
        (chats + sessionChats).firstOrNull { it.id == id }?.withGroupAvatar()
    }

    /** 群头像存在 groupAvatars 里，列表与详情都要现取，否则改完头像列表还是首字母。 */
    private fun Conversation.withGroupAvatar(): Conversation =
        group?.let { copy(group = it.copy(avatarUrl = groupAvatars[it.id])) } ?: this

    /** mock 不真推送：只记下注册过的令牌，方便测试断言「登录时注册、退出时注销」。 */
    var registeredPushToken: String? = null
        private set

    override suspend fun registerPush(token: String, locale: String) = gate {
        registeredPushToken = token
    }

    override suspend fun unregisterPush() = gate {
        registeredPushToken = null
    }

    override suspend fun messages(conversationId: Long, before: Long?, limit: Int): MessagesPage = gate {
        val all = messageStore[conversationId].orEmpty()
        val filtered = if (before == null) all else all.filter { it.id < before }
        MessagesPage(filtered.takeLast(limit), hasMore = filtered.size > limit)
    }

    override suspend fun sendMessage(
        conversationId: Long,
        body: String,
        clientId: String,
        attachments: List<Attachment>,
    ): Message = gate {
        val msg = Message(
            id = (messageStore.values.flatten().maxOfOrNull { it.id } ?: 0) + 1,
            conversationId = conversationId,
            body = body,
            senderName = me.name,
            mine = true,
            createdAt = System.currentTimeMillis(),
            clientId = clientId,
            attachments = attachments,
        )
        messageStore.getOrPut(conversationId) { mutableListOf() }.add(msg)
        if (config.value.fakeReply) {
            scope.launch {
                delay(600)
                val peerName = chats.firstOrNull { it.id == conversationId }?.peer?.name ?: "bob"
                val reply = Message(
                    id = (messageStore.values.flatten().maxOfOrNull { it.id } ?: 0) + 1,
                    conversationId = conversationId,
                    body = "Got it.",
                    senderName = peerName,
                    mine = false,
                    createdAt = System.currentTimeMillis(),
                )
                messageStore[conversationId]?.add(reply)
                changeFlow.emit(ChangeArea.Messages)
            }
        }
        msg
    }

    /**
     * mock 不真上传：把本机 uri 原样回填到 Attachment，界面继续用 Coil 预览。
     * 延迟与错误注入沿用统一的 gate()，与其他接口表现一致。
     */
    override suspend fun uploadAttachment(localUri: String, name: String, mimeType: String): Attachment = gate {
        Attachment(
            id = "file-${System.nanoTime()}",
            name = name,
            mimeType = mimeType,
            localUri = localUri,
        )
    }

    /** mock 里附件本来就带着本地路径，不需要下载。 */
    override suspend fun downloadAttachment(id: String, name: String): String? = null

    override suspend fun markRead(agentId: String) = gate {
        var changed = false
        val idx = chats.indexOfFirst { it.agentId == agentId }
        if (idx >= 0 && chats[idx].pending != 0) {
            chats[idx] = chats[idx].copy(pending = 0)
            changed = true
        }
        val sidx = sessions.indexOfFirst { it.id == agentId }
        if (sidx >= 0 && sessions[sidx].pending != 0) {
            sessions[sidx] = sessions[sidx].copy(pending = 0)
            changed = true
        }
        // 不清变更事件的话，退回列表时角标还是旧值
        if (changed) {
            changeFlow.emit(ChangeArea.Conversations)
            changeFlow.emit(ChangeArea.Agents)
        }
    }

    /** 打开普通会话时清零未读，并通知列表刷新角标。 */
    override suspend fun markConversationRead(conversationId: Long, upTo: Long) = gate {
        val idx = chats.indexOfFirst { it.id == conversationId }
        if (idx >= 0 && chats[idx].pending != 0) {
            chats[idx] = chats[idx].copy(pending = 0)
            changeFlow.emit(ChangeArea.Conversations)
        }
    }

    override suspend fun agents(): List<AgentSession> = gate { if (dropIfEmpty()) emptyList() else sessions.toList() }

    override suspend fun contacts(): List<Contact> = gate {
        if (dropIfEmpty()) emptyList() else contactList.filterNot { c -> blockedList.any { b -> b.id == c.id } }
    }

    override suspend fun requests(): List<ContactRequest> = gate { if (dropIfEmpty()) emptyList() else requestList.toList() }

    override suspend fun acceptRequest(id: String) = gate {
        val req = requestList.firstOrNull { it.id == id }
        if (req != null) {
            requestList.remove(req)
            // 联系人要用账号 ID，不能沿用请求 ID，否则和列表 key 撞车
            contactList.add(Contact("acc-${req.name}", req.name, req.note, online = false))
        }
        changeFlow.emit(ChangeArea.Requests)
    }

    override suspend fun declineRequest(id: String) = gate {
        requestList.removeAll { it.id == id }
        changeFlow.emit(ChangeArea.Requests)
    }

    override suspend fun addContact(accountId: String, note: String?) = gate { Unit }

    /** 按邮箱加人：mock 里直接用 @ 前面的部分当名字，走同一条待接受请求。 */
    override suspend fun addContactByEmail(email: String, note: String?) = gate {
        val target = email.trim()
        if (!target.contains('@') || target.endsWith("@")) {
            throw MockException("Enter a valid email address")
        }
        val name = target.substringBefore('@')
        if (contactList.any { it.name.equals(name, ignoreCase = true) }) {
            throw MockException("They're already your contact.")
        }
        requestList.add(ContactRequest("req-email-$name", name, target))
        changeFlow.emit(ChangeArea.Requests)
    }

    override suspend fun removeContact(accountId: String) = gate {
        contactList.removeAll { it.id == accountId }
        changeFlow.emit(ChangeArea.Contacts)
    }

    /** 只改我这边的会话标题，和真实服务端 PUT /v1/agents/{id}/title 一致。 */
    override suspend fun renameAgent(agentId: String, title: String) = gate {
        val trimmed = title.trim()
        if (trimmed.isEmpty()) throw MockException("The name can't be empty.")
        val idx = sessions.indexOfFirst { it.id == agentId }
        if (idx >= 0) sessions[idx] = sessions[idx].copy(title = trimmed)
        val cidx = chats.indexOfFirst { it.agentId == agentId }
        if (cidx >= 0) {
            val chat = chats[cidx]
            chats[cidx] = chat.copy(peer = chat.peer?.copy(name = trimmed))
        }
        changeFlow.emit(ChangeArea.Agents)
        changeFlow.emit(ChangeArea.Conversations)
    }

    override suspend fun transferGroupOwner(groupId: String, accountId: String) = gate {
        requireGroup(groupId)
        if (groupOwners[groupId] != me.id) throw MockException("Only the owner can transfer this group.")
        if (groupMembers[groupId]?.any { it.id == accountId } != true) {
            throw MockException("They're not in this group.")
        }
        groupOwners[groupId] = accountId
        changeFlow.emit(ChangeArea.Conversations)
    }

    override suspend fun createGroup(name: String, memberIds: List<String>, sessions: List<String>): Conversation = gate {
        val groupId = "grp-${System.nanoTime()}"
        val members = mutableListOf(Contact(me.id, me.name))
        memberIds.forEach { id ->
            contactList.firstOrNull { it.id == id }?.let { members.add(it) }
        }
        groupMembers[groupId] = members
        groupOwners[groupId] = me.id
        val conversation = Conversation(
            // 必须跳过 agent 会话占用的 id（sessionChats 用 5），否则新群会读到别人的消息
            id = ((chats.map { it.id } + sessionChats.map { it.id }).maxOrNull() ?: 0) + 1,
            kind = "group",
            group = Profile(groupId, name, "${members.size} members"),
            lastMessage = LastMessage("Group created", me.name, System.currentTimeMillis(), true),
        )
        chats.add(0, conversation)
        changeFlow.emit(ChangeArea.Conversations)
        conversation
    }

    // ------------------------------------------------------------ group detail / management

    override suspend fun groupDetail(groupId: String): GroupDetail = gate {
        val conversation = chats.firstOrNull { it.group?.id == groupId }
            ?: throw MockException("No such group.")
        GroupDetail(
            id = groupId,
            name = conversation.group?.name.orEmpty(),
            members = groupMembers[groupId].orEmpty().toList(),
            ownerId = groupOwners[groupId],
            myId = me.id,
            avatarUrl = groupAvatars[groupId],
            mySessionIds = groupSessions[groupId].orEmpty().toList(),
        )
    }

    /** 群 → 我选的会话 id；mock 里默认把唯一的会话选中，和真实账号的常见状态一致。 */
    private val groupSessions = mutableMapOf<String, MutableSet<String>>()

    override suspend fun setGroupSessions(groupId: String, agentIds: List<String>) = gate {
        requireGroup(groupId)
        val mine = sessions.map { it.id }.toSet()
        if (agentIds.any { it !in mine }) throw MockException("Those sessions aren't yours.")
        groupSessions[groupId] = agentIds.toMutableSet()
        changeFlow.emit(ChangeArea.Conversations)
    }

    /** 改名后同步对话列表与联系人里的那一行，两处列表都会重新拉取。 */
    override suspend fun renameGroup(groupId: String, name: String) = gate {
        val trimmed = name.trim()
        if (trimmed.isBlank()) throw MockException("Group names can't be empty.")
        requireGroup(groupId)
        if (groupOwners[groupId] != me.id) throw MockException("Only the owner can rename this group.")
        val subtitle = "${groupMembers[groupId]?.size ?: 0} members"
        val index = chats.indexOfFirst { it.group?.id == groupId }
        if (index >= 0) {
            val chat = chats[index]
            chats[index] = chat.copy(group = chat.group?.copy(name = trimmed, subtitle = subtitle))
        }
        val contactIndex = contactList.indexOfFirst { it.id == groupId }
        if (contactIndex >= 0) {
            contactList[contactIndex] = contactList[contactIndex].copy(name = trimmed, subtitle = subtitle)
        }
        changeFlow.emit(ChangeArea.Conversations)
        changeFlow.emit(ChangeArea.Contacts)
    }

    override suspend fun inviteToGroup(groupId: String, accountId: String) = gate {
        requireGroup(groupId)
        val members = groupMembers.getOrPut(groupId) { mutableListOf() }
        if (members.any { it.id == accountId }) throw MockException("They're already in this group.")
        // 群自身也躺在联系人列表里，不能当成可邀请的联系人
        val contact = contactList.firstOrNull { it.id == accountId && it.id != groupId }
            ?: throw MockException("Only your contacts can be invited.")
        members.add(contact)
        syncMemberCount(groupId)
        changeFlow.emit(ChangeArea.Conversations)
        changeFlow.emit(ChangeArea.Contacts)
    }

    override suspend fun removeFromGroup(groupId: String, accountId: String) = gate {
        requireGroup(groupId)
        if (groupOwners[groupId] != me.id) throw MockException("Only the owner can remove members.")
        if (accountId == groupOwners[groupId]) throw MockException("The owner can't be removed.")
        val members = groupMembers[groupId] ?: mutableListOf()
        if (!members.removeAll { it.id == accountId }) throw MockException("They're not in this group.")
        syncMemberCount(groupId)
        changeFlow.emit(ChangeArea.Conversations)
        changeFlow.emit(ChangeArea.Contacts)
    }

    /**
     * mock 只有 alice 一个视角：退群后这个群从她的对话与联系人里消失。
     * 真实服务端只把她自己移出成员，其他成员仍然保留这个群。
     */
    override suspend fun leaveGroup(groupId: String) = gate {
        requireGroup(groupId)
        // 和服务端一致：群主必须先把群转让出去或解散，不能直接退
        if (groupOwners[groupId] == me.id) {
            throw MockException("The owner can't leave. Transfer the group or dissolve it first.")
        }
        dropGroup(groupId)
        changeFlow.emit(ChangeArea.Conversations)
        changeFlow.emit(ChangeArea.Contacts)
    }

    override suspend fun dissolveGroup(groupId: String) = gate {
        requireGroup(groupId)
        if (groupOwners[groupId] != me.id) throw MockException("Only the owner can dissolve this group.")
        dropGroup(groupId)
        changeFlow.emit(ChangeArea.Conversations)
        changeFlow.emit(ChangeArea.Contacts)
    }

    /** 测试用：把群主换成别人，用来覆盖「非群主可以退群」这条路径。 */
    fun setGroupOwner(groupId: String, accountId: String) {
        groupOwners[groupId] = accountId
    }

    private fun requireGroup(groupId: String) {
        if (chats.none { it.group?.id == groupId }) throw MockException("No such group.")
    }

    /** 成员数变化后同步群在对话列表和联系人里的副标题。 */
    private fun syncMemberCount(groupId: String) {
        val subtitle = "${groupMembers[groupId]?.size ?: 0} members"
        val index = chats.indexOfFirst { it.group?.id == groupId }
        if (index >= 0) {
            val chat = chats[index]
            chats[index] = chat.copy(group = chat.group?.copy(subtitle = subtitle))
        }
        val contactIndex = contactList.indexOfFirst { it.id == groupId }
        if (contactIndex >= 0) contactList[contactIndex] = contactList[contactIndex].copy(subtitle = subtitle)
    }

    /** 退群 / 解散：会话、联系人里的群行、成员与消息一起清掉。 */
    private fun dropGroup(groupId: String) {
        val conversationId = chats.firstOrNull { it.group?.id == groupId }?.id
        chats.removeAll { it.group?.id == groupId }
        contactList.removeAll { it.id == groupId }
        groupMembers.remove(groupId)
        groupOwners.remove(groupId)
        if (conversationId != null) messageStore.remove(conversationId)
    }

    override suspend fun devices(): List<Device> = gate { if (dropIfEmpty()) emptyList() else deviceList.toList() }

    override suspend fun revokeDevice(id: String) = gate { deviceList.removeAll { it.id == id }; Unit }

    override suspend fun blocked(): List<BlockedAccount> = gate { blockedList.toList() }

    /** 拉黑后该账号的对话与联系人都不再展示，与真实服务端行为一致。 */
    override suspend fun block(accountId: String) = gate {
        if (blockedList.none { it.id == accountId }) {
            val name = (chats.mapNotNull { it.peer } + contactList.map { Profile(it.id, it.name) })
                .firstOrNull { it.id == accountId }?.name ?: accountId
            blockedList.add(BlockedAccount(accountId, name))
            changeFlow.emit(ChangeArea.All)
        }
    }

    override suspend fun unblock(accountId: String) = gate { blockedList.removeAll { it.id == accountId }; Unit }

    override suspend fun billing(): Billing = gate {
        Billing(access = me.access, trialEndsInDays = me.trialDaysLeft, plansEnabled = true)
    }

    private val reports = mutableListOf<Pair<String, String>>()

    override suspend fun report(targetId: String, reason: String): Unit = gate {
        reports.add(targetId to reason)
        Unit
    }

    /** 调试面板用：切换场景后让界面重新拉一次。 */
    fun poke(area: ChangeArea = ChangeArea.All) {
        scope.launch { changeFlow.emit(area) }
    }
}

class MockException(message: String) : Exception(message)
