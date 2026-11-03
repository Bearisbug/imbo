package dev.imbo.android.data.remote

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * 服务端 DTO，字段与 imbo-proto/src/api.rs 一一对应。
 * 只声明这个 App 用得到的字段，其余靠 ignoreUnknownKeys 忽略。
 */
@Serializable
data class AccountDto(
    val id: Int,
    val name: String,
    val email: String? = null,
    @SerialName("avatar_url") val avatarUrl: String? = null,
    val admin: Boolean = false,
)

/** PUT /v1/me/avatar 的返回（api.rs 的 AvatarResponse）。 */
@Serializable
data class AvatarResponseDto(@SerialName("avatar_url") val avatarUrl: String)

/** POST /v1/me/email 的返回（api.rs 的 AccountResponse）。 */
@Serializable
data class AccountResponseDto(val account: AccountDto)

@Serializable
data class ProfileDto(
    val id: Int,
    val name: String,
    @SerialName("avatar_url") val avatarUrl: String? = null,
)

@Serializable
data class AccountRefDto(val id: Int, val name: String)

@Serializable
data class GroupRefDto(val id: Int, val name: String)

@Serializable
data class AgentRefDto(
    val id: String,
    @SerialName("short_id") val shortId: String,
    val title: String,
    val tool: String,
)

@Serializable
data class SenderDto(
    @SerialName("account_id") val accountId: Int,
    val name: String,
    val agent: AgentRefDto? = null,
    val via: String? = null,
)

@Serializable
data class CodeResponseDto(@SerialName("retry_after_s") val retryAfterS: Long = 0)

@Serializable
data class AppLoginRequestDto(
    val email: String,
    val code: String,
    val name: String? = null,
    val platform: String,
)

@Serializable
data class AppLoginResponseDto(
    val account: AccountDto,
    val token: String,
    @SerialName("new_account") val newAccount: Boolean,
)

@Serializable
data class AccessDto(val state: String, val until: String? = null)

@Serializable
data class DeviceRefDto(val id: String, val name: String)

@Serializable
data class MeResponseDto(
    val account: AccountDto,
    val device: DeviceRefDto? = null,
    val access: AccessDto? = null,
)

@Serializable
data class DeviceDto(
    val id: String,
    val name: String,
    val platform: String,
    @SerialName("created_at") val createdAt: String,
    @SerialName("last_seen_at") val lastSeenAt: String? = null,
    val current: Boolean = false,
)

@Serializable
data class DevicesResponseDto(val devices: List<DeviceDto>)

@Serializable
data class LastMessageDto(
    val id: Long,
    val from: SenderDto,
    val preview: String,
    @SerialName("created_at") val createdAt: String,
)

@Serializable
data class ConversationDto(
    val id: Long,
    val kind: String,
    val peer: ProfileDto? = null,
    val group: ProfileDto? = null,
    @SerialName("last_message") val lastMessage: LastMessageDto? = null,
    val pending: Long = 0,
)

@Serializable
data class ConversationsResponseDto(val conversations: List<ConversationDto>)

@Serializable
data class ConversationRefDto(
    val kind: String,
    val id: Long,
    val group: GroupRefDto? = null,
    val peer: AccountRefDto? = null,
)

@Serializable
data class MessageDto(
    val id: Long,
    val from: SenderDto,
    val conversation: ConversationRefDto? = null,
    val body: String,
    @SerialName("created_at") val createdAt: String,
    @SerialName("from_me") val fromMe: Boolean = false,
    val state: String? = null,
    @SerialName("files") val files: List<FileViewDto> = emptyList(),
)

/** 附件的服务端视图（imbo-proto FileView）；mime 类型服务端可能不返回，按扩展名兜底。 */
@Serializable
data class FileViewDto(
    val id: String,
    val name: String,
    val size: Long = 0,
    @SerialName("mime_type") val mimeType: String? = null,
)

/** POST /v1/files 的返回；quota 当前用不到，靠 ignoreUnknownKeys 忽略。 */
@Serializable
data class UploadResponseDto(val file: FileViewDto)

@Serializable
data class MessagesPageDto(val messages: List<MessageDto>, @SerialName("has_more") val hasMore: Boolean = false)

@Serializable
data class AgentDto(
    val id: String,
    @SerialName("short_id") val shortId: String,
    val title: String,
    val tool: String,
    val online: Boolean = false,
    val unread: Long = 0,
    @SerialName("device_name") val deviceName: String? = null,
    @SerialName("last_online_at") val lastOnlineAt: String? = null,
    val wakeable: Boolean = false,
)

@Serializable
data class AgentsResponseDto(val agents: List<AgentDto>)

@Serializable
data class SessionInfoDto(
    @SerialName("short_id") val shortId: String,
    val title: String,
    val tool: String,
    val online: Boolean = false,
)

@Serializable
data class ContactDto(
    val id: Int,
    val name: String,
    val sessions: List<SessionInfoDto> = emptyList(),
    val active: Boolean = true,
    val blocked: Boolean = false,
)

@Serializable
data class ContactsResponseDto(val contacts: List<ContactDto>)

@Serializable
data class RequestDto(
    val id: Long,
    val kind: String,
    val from: AccountRefDto,
    val to: AccountRefDto,
    val group: GroupRefDto? = null,
    val note: String? = null,
    val status: String,
)

@Serializable
data class RequestsResponseDto(val requests: List<RequestDto>, @SerialName("notes_hidden") val notesHidden: Boolean = false)

@Serializable
data class BlockItemDto(val account: ProfileDto)

@Serializable
data class BlocksResponseDto(val blocks: List<BlockItemDto>)

@Serializable
data class GroupDto(
    val id: Int,
    val name: String,
    @SerialName("avatar_url") val avatarUrl: String? = null,
    val owner: AccountRefDto? = null,
    val members: List<MemberRefDto> = emptyList(),
    /** 我选来接收这个群消息的会话。 */
    @SerialName("my_sessions") val mySessions: List<AgentRefDto> = emptyList(),
)

@Serializable
data class MemberRefDto(
    val id: Int,
    val name: String,
    @SerialName("avatar_url") val avatarUrl: String? = null,
    val active: Boolean = true,
)

@Serializable
data class GroupResponseDto(val group: GroupDto)

/** PUT /v1/groups/{id}/sessions 的正文与返回（api.rs 的 SessionsBody / MySessionsResponse）。 */
@Serializable
data class GroupSessionsDto(val sessions: List<String>)

@Serializable
data class MySessionsDto(@SerialName("my_sessions") val mySessions: List<AgentRefDto> = emptyList())

@Serializable
data class GroupsResponseDto(val groups: List<GroupDto>)

@Serializable
data class InviteBodyDto(@SerialName("account_id") val accountId: Int)

@Serializable
data class BillingResponseDto(
    val enabled: Boolean = false,
    val access: AccessDto? = null,
    @SerialName("trial_ends_at") val trialEndsAt: String? = null,
)

@Serializable
data class SendBodyDto(
    val to: String,
    val body: String,
    @SerialName("client_msg_id") val clientMsgId: String,
    /** 服务端 SendBody.files：先前 POST /v1/files 拿到的文件 id。 */
    val files: List<String> = emptyList(),
)

@Serializable
data class SendResponseDto(
    @SerialName("message_id") val messageId: Long,
    val conversation: ConversationRefDto? = null,
)

@Serializable
data class SendToAgentBodyDto(
    val body: String,
    @SerialName("client_msg_id") val clientMsgId: String,
    val files: List<String> = emptyList(),
)

@Serializable
data class BlockBodyDto(@SerialName("account_id") val accountId: Int)

@Serializable
data class ReportBodyDto(
    @SerialName("account_id") val accountId: Int,
    val reason: String,
    val note: String? = null,
    @SerialName("group_id") val groupId: Int? = null,
)

@Serializable
data class CreateGroupBodyDto(val name: String, val sessions: List<String>? = null)

@Serializable
data class CodeRequestDto(val email: String)

@Serializable
data class ChangeEmailDto(val email: String, val code: String)

/** DELETE /v1/account 要求正文里明确写 DELETE（API-052）。 */
@Serializable
data class DeleteAccountDto(val confirm: String)

/** PUT /v1/app/push 的正文（api.rs 的 PushTokenRequest）。 */
@Serializable
data class PushTokenDto(
    @SerialName("push_token") val pushToken: String,
    val locale: String,
)

@Serializable
data class RenameDto(val name: String)

@Serializable
data class ContactRequestDto(@SerialName("account_id") val accountId: Int? = null, val email: String? = null)

/** RFC 7807，服务端所有错误的形状。 */
@Serializable
data class ProblemDto(
    val type: String = "",
    val title: String = "",
    val status: Int = 0,
    val detail: String = "",
)

@Serializable
data class MessageResponseDto(val message: MessageDto)

@Serializable
data class TitleBodyDto(val title: String)

@Serializable
data class OwnerBodyDto(@SerialName("account_id") val accountId: Int)

@Serializable
data class AgentResponseDto(val agent: AgentDto)
@Serializable
data class ShareBodyDto(
    val share: String,
    val devices: List<String>? = null,
    val sessions: List<String>? = null,
)

@Serializable
data class ShareDeviceDto(val id: String, val name: String)

@Serializable
data class ShareSessionDto(
    val id: String,
    @SerialName("short_id") val shortId: String,
    val title: String,
    val tool: String,
    @SerialName("device_id") val deviceId: String? = null,
    val online: Boolean = false,
)

@Serializable
data class ShareResponseDto(
    val share: String,
    val devices: List<ShareDeviceDto> = emptyList(),
    val sessions: List<ShareSessionDto> = emptyList(),
)
