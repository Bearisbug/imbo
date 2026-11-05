package dev.imbo.android.ui.screen

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChevronLeft
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.imbo.android.R
import dev.imbo.android.data.ChangeArea
import dev.imbo.android.data.Contact
import dev.imbo.android.data.GroupDetail
import dev.imbo.android.data.ImboApi
import dev.imbo.android.data.UiState
import dev.imbo.android.ui.components.Avatar
import dev.imbo.android.ui.components.AvatarImage
import dev.imbo.android.ui.components.GhostButton
import dev.imbo.android.ui.components.Hairline
import dev.imbo.android.ui.components.IconButton32
import dev.imbo.android.ui.components.ImboDialog
import dev.imbo.android.ui.components.ImboTextFieldDialog
import dev.imbo.android.ui.components.ScreenRoot
import dev.imbo.android.ui.components.SectionLabel
import dev.imbo.android.ui.components.SelectionDot
import dev.imbo.android.ui.theme.ImboColor
import dev.imbo.android.ui.theme.ImboDimen
import dev.imbo.android.ui.theme.ImboMono
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

// ---------------------------------------------------------------- state

/** 写操作的类型，用来在 pending / 成功提示里选中对应文案。 */
enum class GroupAction { Rename, Invite, Remove, Transfer, Avatar, Sessions, Leave, Dissolve }

/** 群信息页的写操作状态：互斥，界面据此显示 pending、成功或失败。 */
sealed interface GroupActionState {
    data object Idle : GroupActionState
    data class Working(val action: GroupAction) : GroupActionState
    data class Succeeded(val action: GroupAction, val name: String? = null) : GroupActionState
    /** [action] 记住是哪一步失败：群头像失败要换成固定文案，不把服务端原因直接露给用户。 */
    data class Failed(val message: String, val action: GroupAction? = null) : GroupActionState

    /** 已退出或解散，界面需要离开这一页。 */
    data object Closed : GroupActionState
}

data class GroupInfoUiState(
    val detail: UiState<GroupDetail> = UiState.Loading,
    val contacts: List<Contact> = emptyList(),
    val action: GroupActionState = GroupActionState.Idle,
    /** 我自己的会话，用来选哪些接收这个群的消息。 */
    val agents: List<dev.imbo.android.data.AgentSession> = emptyList(),
)

@HiltViewModel
class GroupInfoViewModel @Inject constructor(private val api: ImboApi) : ViewModel() {

    private val _state = MutableStateFlow(GroupInfoUiState())
    val state: StateFlow<GroupInfoUiState> = _state.asStateFlow()

    private var conversationId: Long = 0
    private var groupId: String? = null

    init {
        // 别处（网页端、另一台设备、群主本人）改了群，本页要跟着变，
        // 否则会在页面上看到过期的群主 / 成员 / 群头像
        viewModelScope.launch {
            api.changes.collect { area ->
                if (area == ChangeArea.All || area == ChangeArea.Conversations) {
                    if (_state.value.action !is GroupActionState.Working) reload()
                }
            }
        }
    }

    fun load(id: Long) {
        if (conversationId == id && _state.value.detail is UiState.Content) return
        conversationId = id
        reload()
    }

    fun reload() {
        viewModelScope.launch {
            // 已有内容时保留，避免操作完刷新时整页闪回 loading
            if (_state.value.detail !is UiState.Content) {
                _state.value = _state.value.copy(detail = UiState.Loading)
            }
            val conversation = try {
                api.conversation(conversationId)
            } catch (e: Exception) {
                _state.value = _state.value.copy(detail = UiState.Error(e.message ?: "error"))
                return@launch
            }
            val group = conversation?.group
            if (group == null) {
                _state.value = _state.value.copy(detail = UiState.Error("This conversation isn't a group."))
                return@launch
            }
            groupId = group.id
            _state.value = try {
                val detail = api.groupDetail(group.id)
                val contacts = runCatching { api.contacts() }.getOrDefault(_state.value.contacts)
                // 选会话要用到我自己的会话列表；拿不到就沿用上一次的
                val agents = runCatching { api.agents() }.getOrDefault(_state.value.agents)
                _state.value.copy(
                    detail = UiState.Content(detail),
                    // 群自身也会出现在联系人列表里，不是可邀请对象
                    contacts = contacts.filterNot { it.id == detail.id },
                    agents = agents,
                )
            } catch (e: Exception) {
                _state.value.copy(detail = UiState.Error(e.message ?: "error"))
            }
        }
    }

    /** 勾选/取消一个会话：把整份列表交给服务端覆盖，成功后刷新回显。 */
    fun setSession(agentId: String, selected: Boolean) {
        val id = groupId ?: return
        val detail = (_state.value.detail as? UiState.Content)?.data ?: return
        val next = if (selected) {
            (detail.mySessionIds + agentId).distinct()
        } else {
            detail.mySessionIds - agentId
        }
        submit(GroupAction.Sessions, request = { api.setGroupSessions(id, next) })
    }

    fun rename(name: String) {
        val id = groupId ?: return
        submit(GroupAction.Rename, request = { api.renameGroup(id, name.trim()) })
    }

    fun invite(contact: Contact) {
        val id = groupId ?: return
        submit(GroupAction.Invite, name = contact.name, request = { api.inviteToGroup(id, contact.id) })
    }

    fun remove(member: Contact) {
        val id = groupId ?: return
        submit(GroupAction.Remove, name = member.name, request = { api.removeFromGroup(id, member.id) })
    }

    /** 只有群主能转让；转完自己变成普通成员，退群入口随之解锁。 */
    fun transfer(member: Contact) {
        val id = groupId ?: return
        submit(GroupAction.Transfer, name = member.name, request = { api.transferGroupOwner(id, member.id) })
    }

    fun leave() {
        val id = groupId ?: return
        submit(GroupAction.Leave, request = { api.leaveGroup(id) })
    }

    fun dissolve() {
        val id = groupId ?: return
        submit(GroupAction.Dissolve, request = { api.dissolveGroup(id) })
    }

    /** 群主换头像：接口直接回新地址，先写回详情，界面不用等下一次刷新。 */
    fun changeAvatar(localUri: String) {
        val id = groupId ?: return
        submit(GroupAction.Avatar, request = { applyAvatarUrl(api.setGroupAvatar(id, localUri)) })
    }

    /** 群主移除头像：服务端只删图不返回详情，成功后本地立刻置空，否则旧图还会挂在界面上。 */
    fun removeAvatar() {
        val id = groupId ?: return
        submit(
            GroupAction.Avatar,
            request = {
                api.clearGroupAvatar(id)
                applyAvatarUrl(null)
            },
        )
    }

    /** 只替换详情里的 avatarUrl，群名和成员不动，避免整页重排。 */
    private fun applyAvatarUrl(url: String?) {
        val current = (_state.value.detail as? UiState.Content)?.data ?: return
        _state.value = _state.value.copy(detail = UiState.Content(current.copy(avatarUrl = url)))
    }

    /** 所有写操作走同一条路径：先进 Working，成功刷新详情，失败保留原因。 */
    private fun submit(action: GroupAction, name: String? = null, request: suspend () -> Unit) {
        if (_state.value.action is GroupActionState.Working) return
        _state.value = _state.value.copy(action = GroupActionState.Working(action))
        viewModelScope.launch {
            try {
                request()
                if (action == GroupAction.Leave || action == GroupAction.Dissolve) {
                    _state.value = _state.value.copy(action = GroupActionState.Closed)
                } else {
                    val success = GroupActionState.Succeeded(action, name)
                    _state.value = _state.value.copy(action = success)
                    // 换头像的返回值就是最终地址，再拉一次详情反而可能用旧数据把它盖回去
                    if (action != GroupAction.Avatar) refreshAfterWrite()
                    delay(2500)
                    if (_state.value.action == success) {
                        _state.value = _state.value.copy(action = GroupActionState.Idle)
                    }
                }
            } catch (e: Exception) {
                _state.value = _state.value.copy(action = GroupActionState.Failed(e.message ?: "error", action))
            }
        }
    }

    private suspend fun refreshAfterWrite() {
        val id = groupId ?: return
        val detail = runCatching { api.groupDetail(id) }.getOrNull() ?: return
        val contacts = runCatching { api.contacts() }.getOrDefault(_state.value.contacts)
        _state.value = _state.value.copy(
            detail = UiState.Content(detail),
            contacts = contacts.filterNot { it.id == detail.id },
        )
    }
}

// ---------------------------------------------------------------- screen

@Composable
fun GroupInfoScreen(
    conversationId: Long,
    onBack: () -> Unit,
    onClosed: () -> Unit,
    viewModel: GroupInfoViewModel = hiltViewModel(),
) {
    LaunchedEffect(conversationId) { viewModel.load(conversationId) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val detail = (state.detail as? UiState.Content)?.data
    val action = state.action
    val busy = action is GroupActionState.Working
    val isOwner = detail?.let { it.ownerId != null && it.ownerId == it.myId } == true
    val uploadingAvatar = (action as? GroupActionState.Working)?.action == GroupAction.Avatar
    val avatarFailed = (action as? GroupActionState.Failed)?.action == GroupAction.Avatar

    // 系统 Photo Picker：不需要存储权限，返回的 content:// 地址交给 api 上传
    val pickGroupAvatar = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri != null) viewModel.changeAvatar(uri.toString())
    }

    LaunchedEffect(action) {
        if (action is GroupActionState.Closed) onClosed()
    }

    var showRename by remember { mutableStateOf(false) }
    var showInvite by remember { mutableStateOf(false) }
    var showTransfer by remember { mutableStateOf(false) }
    var pendingTransfer by remember { mutableStateOf<Contact?>(null) }
    var showLeave by remember { mutableStateOf(false) }
    var showDissolve by remember { mutableStateOf(false) }
    var pendingRemove by remember { mutableStateOf<Contact?>(null) }
    var showRemoveAvatar by remember { mutableStateOf(false) }
    var ownerOnlyHint by remember { mutableStateOf(false) }

    // 「只有群主能换」是点一下的即时反馈，显示一会儿就收起，不占页面
    LaunchedEffect(ownerOnlyHint) {
        if (ownerOnlyHint) {
            delay(2500)
            ownerOnlyHint = false
        }
    }

    ScreenRoot {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(ImboDimen.BrandBar)
                .padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton32(onClick = onBack) {
                Icon(
                    Icons.Outlined.ChevronLeft,
                    contentDescription = stringResource(R.string.back),
                    tint = ImboColor.Tertiary,
                )
            }
            Column(modifier = Modifier.weight(1f).padding(horizontal = 4.dp)) {
                // 群名和成员数由下面的头像块负责，顶栏只写「这是哪一页」，避免同一行信息说两遍
                Text(
                    text = stringResource(R.string.group_info),
                    color = ImboColor.OnBackground,
                    fontFamily = ImboMono,
                    fontSize = 14.sp,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Hairline(vertical = false)
        when (val s = state.detail) {
            is UiState.Loading -> LoadingBlock()
            is UiState.Error -> ErrorBlock(s.message) { viewModel.reload() }
            is UiState.Content -> GroupInfoBody(
                detail = s.data,
                action = action,
                busy = busy,
                uploading = uploadingAvatar,
                // 头像旁边的提示按优先级只显示一条：上传中 > 失败 > 非群主点了一下
                avatarNotice = when {
                    ownerOnlyHint -> stringResource(R.string.group_avatar_owner_only)
                    avatarFailed -> stringResource(R.string.group_avatar_failed)
                    else -> null
                },
                avatarNoticeIsError = avatarFailed && !ownerOnlyHint,
                agents = state.agents,
                onToggleSession = { agentId, on -> viewModel.setSession(agentId, on) },
                onPickPicture = {
                    pickGroupAvatar.launch(
                        PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                    )
                },
                onRemovePicture = { showRemoveAvatar = true },
                onOwnerOnlyTap = { ownerOnlyHint = true },
                onRename = { showRename = true },
                onInvite = { showInvite = true },
                onRemove = { pendingRemove = it },
                onTransfer = { showTransfer = true },
                onLeave = { showLeave = true },
                onDissolve = { showDissolve = true },
            )
        }
    }

    if (showRename && detail != null) {
        ImboTextFieldDialog(
            title = stringResource(R.string.group_rename_title),
            label = stringResource(R.string.group_name_label),
            initial = detail.name,
            confirmLabel = stringResource(R.string.save),
            onConfirm = { name ->
                showRename = false
                viewModel.rename(name)
            },
            onDismiss = { showRename = false },
        )
    }
    if (showInvite && detail != null) {
        InviteContactDialog(
            contacts = state.contacts.filterNot { contact -> detail.members.any { it.id == contact.id } },
            onInvite = { contact ->
                showInvite = false
                viewModel.invite(contact)
            },
            onDismiss = { showInvite = false },
        )
    }
    if (showTransfer && detail != null) {
        TransferOwnerDialog(
            members = detail.members.filterNot { it.id == detail.myId },
            onPick = { member ->
                showTransfer = false
                pendingTransfer = member
            },
            onDismiss = { showTransfer = false },
        )
    }
    pendingTransfer?.let { member ->
        ImboDialog(
            title = stringResource(R.string.transfer_owner_title, member.name),
            body = stringResource(R.string.transfer_owner_body),
            confirmLabel = stringResource(R.string.transfer_owner),
            onConfirm = {
                pendingTransfer = null
                viewModel.transfer(member)
            },
            onDismiss = { pendingTransfer = null },
        )
    }
    if (showLeave) {
        ImboDialog(
            title = stringResource(R.string.group_leave_title),
            body = stringResource(R.string.group_leave_body),
            confirmLabel = stringResource(R.string.group_leave),
            onConfirm = {
                showLeave = false
                viewModel.leave()
            },
            onDismiss = { showLeave = false },
            destructive = true,
        )
    }
    if (showDissolve && isOwner) {
        ImboDialog(
            title = stringResource(R.string.group_dissolve_title),
            body = stringResource(R.string.group_dissolve_body),
            confirmLabel = stringResource(R.string.group_dissolve),
            onConfirm = {
                showDissolve = false
                viewModel.dissolve()
            },
            onDismiss = { showDissolve = false },
            destructive = true,
        )
    }
    pendingRemove?.let { member ->
        ImboDialog(
            title = stringResource(R.string.group_remove_title),
            body = stringResource(R.string.group_remove_body, member.name),
            confirmLabel = stringResource(R.string.group_remove),
            onConfirm = {
                pendingRemove = null
                viewModel.remove(member)
            },
            onDismiss = { pendingRemove = null },
            destructive = true,
        )
    }
    // 长按只在群主头像上绑定，这里再判一次 owner，转让后立刻失效
    if (showRemoveAvatar && detail != null && isOwner) {
        ImboDialog(
            title = detail.name,
            // 没有单独的正文 key，用「移除群头像」本身说清动作，确认按钮复用同一句
            body = stringResource(R.string.group_avatar_remove),
            confirmLabel = stringResource(R.string.group_avatar_remove),
            onConfirm = {
                showRemoveAvatar = false
                viewModel.removeAvatar()
            },
            onDismiss = { showRemoveAvatar = false },
            destructive = true,
        )
    }
}

@Composable
private fun GroupInfoBody(
    detail: GroupDetail,
    action: GroupActionState,
    busy: Boolean,
    uploading: Boolean,
    avatarNotice: String?,
    avatarNoticeIsError: Boolean,
    agents: List<dev.imbo.android.data.AgentSession>,
    onToggleSession: (String, Boolean) -> Unit,
    onPickPicture: () -> Unit,
    onRemovePicture: () -> Unit,
    onOwnerOnlyTap: () -> Unit,
    onRename: () -> Unit,
    onInvite: () -> Unit,
    onRemove: (Contact) -> Unit,
    onTransfer: () -> Unit,
    onLeave: () -> Unit,
    onDissolve: () -> Unit,
) {
    val isOwner = detail.ownerId != null && detail.ownerId == detail.myId
    LazyColumn(modifier = Modifier.fillMaxSize()) {
        item(key = "group-avatar") {
            GroupAvatarHeader(
                detail = detail,
                isOwner = isOwner,
                enabled = !busy,
                uploading = uploading,
                notice = avatarNotice,
                noticeIsError = avatarNoticeIsError,
                onPickPicture = onPickPicture,
                onRemovePicture = onRemovePicture,
                onOwnerOnlyTap = onOwnerOnlyTap,
            )
        }
        item(key = "members-label") { SectionLabel(stringResource(R.string.group_members)) }
        items(detail.members, key = { "member:${it.id}" }) { member ->
            GroupMemberRow(
                member = member,
                detail = detail,
                isOwner = isOwner,
                enabled = !busy,
                onRemove = onRemove,
            )
        }
        // 我的会话：只有被勾选的会话会收到这个群的消息（服务端 PUT /sessions）
        if (agents.isNotEmpty()) {
            item(key = "sessions-label") {
                Spacer(Modifier.height(8.dp))
                SectionLabel(stringResource(R.string.group_my_sessions))
            }
            items(agents, key = { "session:${it.id}" }) { agent ->
                GroupSessionRow(
                    agent = agent,
                    selected = detail.mySessionIds.contains(agent.id),
                    enabled = !busy,
                    onToggle = { onToggleSession(agent.id, it) },
                )
            }
        }
        item(key = "actions-label") {
            Spacer(Modifier.height(8.dp))
            SectionLabel(stringResource(R.string.group_actions))
        }
        item(key = "rename") {
            GroupActionRow(stringResource(R.string.group_rename), enabled = !busy, onClick = onRename)
        }
        item(key = "invite") {
            GroupActionRow(stringResource(R.string.group_invite), enabled = !busy, onClick = onInvite)
        }
        if (isOwner && detail.members.size > 1) {
            item(key = "transfer") {
                GroupActionRow(
                    label = stringResource(R.string.transfer_owner),
                    enabled = !busy,
                    onClick = onTransfer,
                )
            }
        }
        item(key = "leave") {
            // 真实服务端拒绝群主退群（要求先转让或解散），这里提前禁用并说明原因
            GroupActionRow(
                label = stringResource(R.string.group_leave),
                enabled = !busy && !isOwner,
                onClick = onLeave,
            )
        }
        if (isOwner) {
            item(key = "owner_leave_hint") {
                Text(
                    text = stringResource(R.string.group_owner_cannot_leave),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = ImboDimen.Gutter, vertical = 4.dp),
                )
            }
            item(key = "dissolve") {
                GroupActionRow(
                    label = stringResource(R.string.group_dissolve),
                    enabled = !busy,
                    destructive = true,
                    onClick = onDissolve,
                )
            }
        }
        item(key = "feedback") { GroupActionFeedback(action) }
    }
}

/**
 * 群信息页顶部：64dp 群头像 + 群名 + 成员数。
 * 头像整块只有群主能点（换图）和长按（移除），非群主点一下只得到一句说明，不弹任何选择器。
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun GroupAvatarHeader(
    detail: GroupDetail,
    isOwner: Boolean,
    enabled: Boolean,
    uploading: Boolean,
    notice: String?,
    noticeIsError: Boolean,
    onPickPicture: () -> Unit,
    onRemovePicture: () -> Unit,
    onOwnerOnlyTap: () -> Unit,
) {
    val pickLabel = stringResource(R.string.group_avatar_change)
    val ownerOnlyLabel = stringResource(R.string.group_avatar_owner_only)
    // 上传中 / 失败 / 非群主提示优先，其余时候给群主一个「可以换图」的入口说明
    val hint = when {
        uploading -> stringResource(R.string.group_avatar_uploading)
        notice != null -> notice
        isOwner -> pickLabel
        else -> null
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .combinedClickable(
                enabled = enabled,
                onClickLabel = if (isOwner) pickLabel else ownerOnlyLabel,
                role = Role.Button,
                onLongClickLabel = if (isOwner) stringResource(R.string.group_avatar_remove) else null,
                onLongClick = if (isOwner) onRemovePicture else null,
                onClick = { if (isOwner) onPickPicture() else onOwnerOnlyTap() },
            )
            .padding(horizontal = ImboDimen.Gutter, vertical = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            modifier = Modifier
                .size(ImboDimen.AvatarLarge)
                .clip(CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            AvatarImage(
                letter = detail.name.take(1).uppercase(),
                url = detail.avatarUrl,
                size = 64,
                group = true,
            )
            if (uploading) {
                // 盖一层半透明遮罩，明确告诉群主旧图还没被替换
                Box(
                    modifier = Modifier
                        .matchParentSize()
                        .clip(CircleShape)
                        .background(ImboColor.TertiaryContainer.copy(alpha = 0.55f)),
                )
            }
        }
        Column(modifier = Modifier.weight(1f).padding(start = 14.dp)) {
            Text(
                text = detail.name,
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = stringResource(R.string.members_count, detail.members.size),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
            )
            if (hint != null) {
                Text(
                    text = hint,
                    color = if (noticeIsError) ImboColor.Error else ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
        }
    }
}

/** 一行会话：勾上就表示这个群的消息也发给它。 */
@Composable
private fun GroupSessionRow(
    agent: dev.imbo.android.data.AgentSession,
    selected: Boolean,
    enabled: Boolean,
    onToggle: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = enabled) { onToggle(!selected) }
            .padding(horizontal = ImboDimen.Gutter, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(modifier = Modifier.size(20.dp), contentAlignment = Alignment.Center) {
            SelectionDot(selected = selected)
        }
        Column(modifier = Modifier.weight(1f).padding(start = 12.dp)) {
            Text(
                text = agent.title,
                color = if (enabled) ImboColor.OnBackground else ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = agent.tool,
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun GroupMemberRow(
    member: Contact,
    detail: GroupDetail,
    isOwner: Boolean,
    enabled: Boolean,
    onRemove: (Contact) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = ImboDimen.Gutter, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Avatar(letter = member.name.take(1).uppercase())
        Column(modifier = Modifier.weight(1f).padding(start = 12.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Text(
                    text = member.name,
                    color = ImboColor.OnBackground,
                    fontFamily = ImboMono,
                    fontSize = 14.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (member.id == detail.ownerId) MemberTag(stringResource(R.string.group_owner))
                if (member.id == detail.myId) MemberTag(stringResource(R.string.group_you))
            }
            member.subtitle?.takeIf { it.isNotBlank() }?.let {
                Text(
                    text = it,
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (isOwner && member.id != detail.myId) {
            GhostButton(
                text = stringResource(R.string.group_remove),
                onClick = { onRemove(member) },
                enabled = enabled,
            )
        }
    }
}

@Composable
private fun MemberTag(label: String) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(4.dp))
            .background(ImboColor.SecondaryContainer)
            .padding(horizontal = 5.dp, vertical = 1.dp),
    ) {
        Text(
            text = label,
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 10.sp,
        )
    }
}

@Composable
private fun GroupActionRow(
    label: String,
    enabled: Boolean,
    destructive: Boolean = false,
    onClick: () -> Unit,
) {
    Text(
        text = label,
        color = when {
            !enabled -> ImboColor.Tertiary
            destructive -> ImboColor.Error
            else -> ImboColor.OnBackground
        },
        fontFamily = ImboMono,
        fontSize = 14.sp,
        modifier = Modifier
            .fillMaxWidth()
            .then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(horizontal = ImboDimen.Gutter, vertical = 16.dp),
    )
}

@Composable
private fun GroupActionFeedback(action: GroupActionState) {
    // 群头像的上传 / 成功 / 失败都显示在头像旁边，底部不再重复一行
    val avatarAction = when (action) {
        is GroupActionState.Working -> action.action
        is GroupActionState.Succeeded -> action.action
        is GroupActionState.Failed -> action.action
        else -> null
    }
    if (avatarAction == GroupAction.Avatar) return
    val label: Pair<String, Color> = when (action) {
        is GroupActionState.Working -> pendingLabel(action.action) to ImboColor.Tertiary
        is GroupActionState.Succeeded -> successLabel(action) to ImboColor.Success
        is GroupActionState.Failed -> action.message to ImboColor.Error
        else -> return
    }
    Text(
        text = label.first,
        color = label.second,
        fontFamily = ImboMono,
        fontSize = 12.sp,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = ImboDimen.Gutter, vertical = 4.dp),
    )
}

@Composable
private fun pendingLabel(action: GroupAction): String = when (action) {
    GroupAction.Rename -> stringResource(R.string.group_saving)
    GroupAction.Invite -> stringResource(R.string.group_inviting)
    GroupAction.Remove -> stringResource(R.string.group_removing)
    GroupAction.Transfer -> stringResource(R.string.group_transferring)
    GroupAction.Avatar -> stringResource(R.string.group_avatar_uploading)
    GroupAction.Sessions -> stringResource(R.string.group_saving)
    GroupAction.Leave -> stringResource(R.string.group_leaving)
    GroupAction.Dissolve -> stringResource(R.string.group_dissolving)
}

@Composable
private fun successLabel(state: GroupActionState.Succeeded): String = when (state.action) {
    GroupAction.Rename -> stringResource(R.string.group_renamed)
    GroupAction.Invite -> stringResource(R.string.group_invited, state.name.orEmpty())
    GroupAction.Remove -> stringResource(R.string.group_removed, state.name.orEmpty())
    GroupAction.Transfer -> stringResource(R.string.group_transferred, state.name.orEmpty())
    // 换头像成功时头像本身已经变了，不再额外给文字
    GroupAction.Avatar -> ""
    // 选会话是即时生效的勾选，不需要成功提示
    GroupAction.Sessions -> ""
    // 退群 / 解散会直接离开页面，不会走到成功提示
    GroupAction.Leave, GroupAction.Dissolve -> ""
}

/** 邀请面板：只列还没进群的联系人，点一行即发出邀请。 */@Composable
private fun InviteContactDialog(
    contacts: List<Contact>,
    onInvite: (Contact) -> Unit,
    onDismiss: () -> Unit,
) {
    Dialog(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .background(ImboColor.SecondaryContainer)
                .padding(vertical = 4.dp),
        ) {
            Text(
                text = stringResource(R.string.group_invite_title),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            )
            Hairline(vertical = false)
            if (contacts.isEmpty()) {
                Text(
                    text = stringResource(R.string.group_invite_empty),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 16.dp),
                )
            } else {
                // 联系人多时限制高度，保证底部的取消始终可见
                LazyColumn(modifier = Modifier.heightIn(max = 360.dp)) {
                    items(contacts, key = { "invite:${it.id}" }) { contact ->
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { onInvite(contact) }
                                .padding(horizontal = 16.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Avatar(letter = contact.name.take(1).uppercase())
                            Text(
                                text = contact.name,
                                color = ImboColor.OnBackground,
                                fontFamily = ImboMono,
                                fontSize = 14.sp,
                                modifier = Modifier.padding(start = 12.dp),
                            )
                        }
                    }
                }
            }
            Hairline(vertical = false)
            Text(
                text = stringResource(R.string.cancel),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(onClick = onDismiss)
                    .padding(horizontal = 16.dp, vertical = 14.dp),
            )
        }
    }
}

/** 转让面板：列出除自己以外的成员，点一行再进确认，避免误触直接把群交出去。 */
@Composable
private fun TransferOwnerDialog(
    members: List<Contact>,
    onPick: (Contact) -> Unit,
    onDismiss: () -> Unit,
) {
    Dialog(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .background(ImboColor.SecondaryContainer)
                .padding(vertical = 4.dp),
        ) {
            Text(
                text = stringResource(R.string.transfer_owner),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            )
            Hairline(vertical = false)
            if (members.isEmpty()) {
                Text(
                    text = stringResource(R.string.group_transfer_empty),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 16.dp),
                )
            } else {
                LazyColumn(modifier = Modifier.heightIn(max = 360.dp)) {
                    items(members, key = { "transfer:${it.id}" }) { member ->
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { onPick(member) }
                                .padding(horizontal = 16.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Avatar(letter = member.name.take(1).uppercase())
                            Text(
                                text = member.name,
                                color = ImboColor.OnBackground,
                                fontFamily = ImboMono,
                                fontSize = 14.sp,
                                modifier = Modifier.padding(start = 12.dp),
                            )
                        }
                    }
                }
            }
            Hairline(vertical = false)
            Text(
                text = stringResource(R.string.cancel),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(onClick = onDismiss)
                    .padding(horizontal = 16.dp, vertical = 14.dp),
            )
        }
    }
}
