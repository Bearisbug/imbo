package dev.imbo.android.data

/**
 * 与 imbo-server 的 DTO 对齐，字段名沿用服务端返回值，切到真实实现时不需要改模型。
 * 服务端定义见 imbo-proto/src/api.rs。
 */
data class Account(
    val id: String,
    val name: String,
    val email: String,
    val accountId: String,
    val trialDaysLeft: Int,
    val access: Access,
    /** 头像地址：mock 存本地 content:// URI，真实服务端返回图片 URL；空表示用名字首字母。 */
    val avatarUrl: String? = null,
)

/** POST /v1/app/session 的返回：new_account 决定欢迎页文案。 */
data class SessionInfo(
    val account: Account,
    val newAccount: Boolean,
)

/** billing.rs 的 Access 枚举。 */
sealed interface Access {
    data class Trial(val daysLeft: Int) : Access
    data class Paid(val daysLeft: Int) : Access
    data object Expired : Access

    /** 服务端的 free：既没有试用也没有订阅。 */
    data object Free : Access
}

data class Profile(
    val id: String,
    val name: String,
    /** 副标题：agent 会话显示工具名，群显示成员数，单聊显示在线状态。 */
    val subtitle: String? = null,
    val online: Boolean = false,
    val agent: Boolean = false,
    /** 单聊对端与群的头像地址；空表示用名字首字母。 */
    val avatarUrl: String? = null,
)

data class LastMessage(
    val preview: String,
    val fromName: String,
    val createdAt: Long,
    val mine: Boolean,
)

data class Conversation(
    val id: Long,
    /** direct | group */
    val kind: String,
    val peer: Profile? = null,
    val group: Profile? = null,
    val lastMessage: LastMessage? = null,
    /** 未读条数 */
    val pending: Int = 0,
    /** agent 会话才有，用于标记已读 */
    val agentId: String? = null,
) {
    val title: String get() = group?.name ?: peer?.name.orEmpty()
    val isGroup: Boolean get() = kind == "group"
}

/** 一条消息附件。localUri 只在它本来就是本机文件时有值，服务端拉回来的消息没有。 */
data class Attachment(
    val id: String,
    val name: String,
    val mimeType: String,
    val localUri: String? = null,
    val sizeBytes: Long = 0,
)

data class Message(
    val id: Long,
    val conversationId: Long,
    val body: String,
    val senderName: String,
    val mine: Boolean,
    val createdAt: Long,
    /** 客户端生成的稳定 id，乐观回显用；空表示来自服务端 */
    val clientId: String? = null,
    val delivery: Delivery = Delivery.Sent,
    val attachments: List<Attachment> = emptyList(),
    /** 消息从哪来：app / web / terminal / agent（服务端 MessageView 的 via）。 */
    val via: String? = null,
    /** 服务端投递状态：pending / pushed / delivered / canceled。 */
    val serverState: String? = null,
)

enum class Delivery { Sending, Sent, Failed }

data class MessagesPage(
    val messages: List<Message>,
    val hasMore: Boolean,
)

data class Contact(
    val id: String,
    val name: String,
    val subtitle: String? = null,
    val online: Boolean = false,
    val agent: Boolean = false,
)

data class ContactRequest(
    val id: String,
    val name: String,
    val note: String? = null,
    /** contact = 加联系人请求；group = 群邀请。 */
    val kind: String = "contact",
    /** 群邀请时的群名，用来把文案说清楚。 */
    val groupName: String? = null,
)

enum class RequestStatus { Pending, Accepted, Declined, Closed }

data class Device(
    val id: String,
    val name: String,
    val platform: String,
    val lastSeenMinutes: Int,
    val thisDevice: Boolean = false,
)

data class BlockedAccount(
    val id: String,
    val name: String,
)

data class Group(
    val id: String,
    val name: String,
    val memberCount: Int,
)

/** 群详情：对应服务端 GroupView，owner / myId 用来区分群主与本人。 */
data class GroupDetail(
    val id: String,
    val name: String,
    val members: List<Contact>,
    val ownerId: String? = null,
    val myId: String? = null,
    /** 群头像地址；空表示用群名首字母。 */
    val avatarUrl: String? = null,
    /** 我选的、接收这个群消息的会话 id（GroupView.my_sessions）。 */
    val mySessionIds: List<String> = emptyList(),
)

data class AgentSession(
    val id: String,
    val title: String,
    val tool: String,
    val state: String,
    val pending: Int,
    /** 会话跑在哪台电脑上（服务端 AgentView.device_name）。 */
    val deviceName: String? = null,
    val online: Boolean = false,
    /** 最后一次在线时间；离线时显示「x 分钟前在线」用。 */
    val lastOnlineAt: Long? = null,
    /** 电脑在线且版本够新时，从 App 发消息能把它重新拉起来。 */
    val wakeable: Boolean = false,
)

data class Billing(
    val access: Access,
    val trialEndsInDays: Int?,
    val plansEnabled: Boolean,
)

/** mock 通道的可调参数，用来在验收时造出加载 / 空 / 错误 / 慢网。 */
data class MockConfig(
    val latencyMs: Long = 150,
    val forceError: ForceError? = null,
    val emptyData: Boolean = false,
    val fakeReply: Boolean = true,
)

enum class ForceError { Unauthorized, ServerError, Timeout }
/** 一个联系人的会话共享设置（服务端 contacts.share）。 */
data class ContactShare(
    val share: String,
    val devices: List<SharedDevice> = emptyList(),
    val sessions: List<SharedSession> = emptyList(),
)

data class SharedDevice(val id: String, val name: String)

data class SharedSession(
    val id: String,
    val shortId: String,
    val title: String,
    val tool: String,
    val deviceId: String? = null,
    val online: Boolean = false,
    /** chosen 模式下这个会话是否被勾中；由客户端合并候选列表时填。 */
    val selected: Boolean = false,
)
