package dev.imbo.android.data.remote

import dev.imbo.android.data.ContactShare
import dev.imbo.android.data.SharedDevice
import dev.imbo.android.data.SharedSession
import dev.imbo.android.data.Access
import dev.imbo.android.data.Account
import dev.imbo.android.data.AgentSession
import dev.imbo.android.data.Attachment
import dev.imbo.android.data.Billing
import dev.imbo.android.data.BlockedAccount
import dev.imbo.android.data.Contact
import dev.imbo.android.data.ContactRequest
import dev.imbo.android.data.Conversation
import dev.imbo.android.data.Device
import dev.imbo.android.data.GroupDetail
import dev.imbo.android.data.LastMessage
import dev.imbo.android.data.Message
import dev.imbo.android.data.Profile
import java.time.Duration
import java.time.Instant
import java.time.format.DateTimeParseException
import kotlin.math.absoluteValue

/**
 * 服务端 DTO → App 模型。只在这里做字段改名和类型收窄，
 * 界面层不感知服务端的 i32/UUID/ISO 时间。
 */
object Mapper {

    fun instantOrZero(iso: String?): Long = try {
        if (iso.isNullOrBlank()) 0L else Instant.parse(iso).toEpochMilli()
    } catch (e: DateTimeParseException) {
        0L
    }

    fun daysUntil(iso: String?): Int = try {
        if (iso.isNullOrBlank()) 0 else {
            val days = Duration.between(Instant.now(), Instant.parse(iso)).toDays().toInt()
            days.coerceAtLeast(0)
        }
    } catch (e: DateTimeParseException) {
        0
    }

    fun access(dto: AccessDto?): Access = when (dto?.state) {
        "paid" -> Access.Paid(daysUntil(dto.until))
        "trial" -> Access.Trial(daysUntil(dto.until))
        "expired" -> Access.Expired
        else -> Access.Free
    }

    fun account(dto: AccountDto, accessDto: AccessDto?): Account {
        val access = access(dto = accessDto)
        val days = when (access) {
            is Access.Paid -> access.daysLeft
            is Access.Trial -> access.daysLeft
            else -> 0
        }
        return Account(
            id = dto.id.toString(),
            name = dto.name,
            email = dto.email.orEmpty(),
            accountId = dto.id.toString(),
            trialDaysLeft = days,
            access = access,
            avatarUrl = dto.avatarUrl,
        )
    }

    fun profile(dto: ProfileDto): Profile = Profile(
        id = dto.id.toString(),
        name = dto.name,
        subtitle = null,
        online = false,
        agent = false,
        avatarUrl = dto.avatarUrl,
    )

    fun conversation(dto: ConversationDto, myId: Int): Conversation = Conversation(
        id = dto.id,
        kind = dto.kind,
        peer = dto.peer?.let { profile(it) },
        group = dto.group?.let { profile(it) },
        lastMessage = dto.lastMessage?.let {
            LastMessage(
                preview = it.preview,
                fromName = it.from.name,
                createdAt = instantOrZero(it.createdAt),
                mine = it.from.accountId == myId,
            )
        },
        pending = dto.pending.toInt(),
        agentId = null,
    )

    fun message(dto: MessageDto, conversationId: Long, myId: Int): Message = Message(
        id = dto.id,
        conversationId = conversationId,
        body = dto.body,
        senderName = dto.from.agent?.title ?: dto.from.name,
        // 自己账号发的、但来自 agent 会话的消息不是「我」说的，标签写会话名
        mine = dto.fromMe || (dto.from.accountId == myId && dto.from.agent == null),
        createdAt = instantOrZero(dto.createdAt),
        clientId = null,
        delivery = dev.imbo.android.data.Delivery.Sent,
        attachments = dto.files.map { attachment(it) },
        via = dto.from.via,
        serverState = dto.state,
    )

    /** 服务端的 FileView 不含 MIME 类型时，按文件名扩展名推断，图片判断才不会误判。 */
    fun attachment(dto: FileViewDto, localUri: String? = null, mimeType: String? = null): Attachment =
        Attachment(
            id = dto.id,
            name = dto.name,
            mimeType = mimeType ?: dto.mimeType ?: mimeTypeFor(dto.name),
            localUri = localUri,
            sizeBytes = dto.size,
        )

    fun mimeTypeFor(name: String): String =
        java.net.URLConnection.guessContentTypeFromName(name) ?: "application/octet-stream"

    fun agentSession(dto: AgentDto): AgentSession = AgentSession(
        id = dto.id,
        title = dto.title,
        tool = dto.tool,
        state = if (dto.online) "online" else "offline",
        pending = dto.unread.toInt(),
        deviceName = dto.deviceName,
        online = dto.online,
        lastOnlineAt = instantOrZero(dto.lastOnlineAt).takeIf { it > 0 },
        wakeable = dto.wakeable,
    )

    /**
     * 会话单聊在服务端不是 conversation，而是 agent 的收件地址 `<账号ID>/<short_id>`。
     * 这里给它一个稳定的负 id，界面沿用同一套 chat 路由。
     */
    fun agentConversationId(uuid: String): Long {
        val hash = uuid.hashCode().toLong().absoluteValue % 1_000_000L
        return -(hash + 1)
    }

    fun agentConversation(dto: AgentDto, myAccountId: Int): Conversation = Conversation(
        id = agentConversationId(dto.id),
        kind = "direct",
        peer = Profile(
            id = dto.id,
            name = dto.title,
            subtitle = dto.tool,
            online = dto.online,
            agent = true,
        ),
        group = null,
        lastMessage = null,
        pending = dto.unread.toInt(),
        agentId = dto.id,
    )

    fun agentAddress(shortId: String, myAccountId: Int): String = "$myAccountId/$shortId"

    fun contact(dto: ContactDto): Contact = Contact(
        id = dto.id.toString(),
        name = dto.name,
        subtitle = dto.sessions.firstOrNull()?.tool,
        online = dto.sessions.any { it.online },
        agent = false,
    )

    fun request(dto: RequestDto): ContactRequest = ContactRequest(
        id = dto.id.toString(),
        name = dto.from.name,
        note = dto.note,
        kind = dto.kind,
        groupName = dto.group?.name,
    )

    /** GroupView → GroupDetail：owner 缺省表示旧服务端还没有群主字段。 */
    fun groupDetail(dto: GroupDto, myId: Int): GroupDetail = GroupDetail(
        id = dto.id.toString(),
        name = dto.name,
        members = dto.members.map { member ->
            Contact(
                id = member.id.toString(),
                name = member.name,
            )
        },
        ownerId = dto.owner?.id?.toString(),
        myId = myId.toString(),
        avatarUrl = dto.avatarUrl,
        mySessionIds = dto.mySessions.map { it.id },
    )

    fun device(dto: DeviceDto): Device = Device(
        id = dto.id,
        name = dto.name,
        platform = dto.platform,
        lastSeenMinutes = minutesSince(dto.lastSeenAt ?: dto.createdAt),
        thisDevice = dto.current,
    )

    private fun minutesSince(iso: String?): Int {
        val ts = instantOrZero(iso)
        if (ts == 0L) return 0
        return ((System.currentTimeMillis() - ts) / 60_000L).toInt().coerceAtLeast(0)
    }

    fun block(dto: BlockItemDto): BlockedAccount = BlockedAccount(
        id = dto.account.id.toString(),
        name = dto.account.name,
    )

    fun billing(dto: BillingResponseDto): Billing {
        val access = access(dto.access)
        val trialDays = when (access) {
            is Access.Paid -> access.daysLeft
            is Access.Trial -> access.daysLeft
            else -> null
        }
        return Billing(
            access = access,
            trialEndsInDays = trialDays ?: if (dto.trialEndsAt != null) daysUntil(dto.trialEndsAt) else null,
            plansEnabled = dto.enabled,
        )
    }

    fun contactShare(dto: ShareResponseDto): ContactShare = ContactShare(
        share = dto.share,
        devices = dto.devices.map { SharedDevice(it.id, it.name) },
        sessions = dto.sessions.map {
            SharedSession(it.id, it.shortId, it.title, it.tool, it.deviceId, it.online)
        },
    )
}
