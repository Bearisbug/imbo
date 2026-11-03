package dev.imbo.android.ui.screen

import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.imbo.android.R
import dev.imbo.android.data.AgentSession
import dev.imbo.android.data.Contact
import dev.imbo.android.data.ContactRequest
import dev.imbo.android.data.Conversation
import dev.imbo.android.data.ImboApi
import dev.imbo.android.data.UiState
import dev.imbo.android.push.Notifications
import dev.imbo.android.push.PushRegistrar
import dev.imbo.android.ui.components.Avatar
import dev.imbo.android.ui.components.BrandBar
import dev.imbo.android.ui.components.ConversationRow
import dev.imbo.android.ui.components.EmptyState
import dev.imbo.android.ui.components.GhostButton
import dev.imbo.android.ui.components.ImboTabs
import dev.imbo.android.ui.components.InsetField
import dev.imbo.android.ui.components.ImboTextFieldDialog
import dev.imbo.android.ui.components.ScreenRoot
import dev.imbo.android.ui.components.SectionLabel
import dev.imbo.android.ui.components.localizedServerError
import dev.imbo.android.ui.components.relativeTime
import dev.imbo.android.ui.theme.ImboColor
import dev.imbo.android.ui.theme.ImboDimen
import dev.imbo.android.ui.theme.ImboMono
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class ChatsViewModel @Inject constructor(
    private val api: ImboApi,
    private val push: PushRegistrar,
) : ViewModel() {
    private val _state = MutableStateFlow<UiState<List<Conversation>>>(UiState.Loading)
    val state: StateFlow<UiState<List<Conversation>>> = _state.asStateFlow()

    init {
        refresh()
        viewModelScope.launch { api.changes.collect { refresh() } }
        // 对话列表是登录后的落地页：在这里注册推送令牌，登录成功就能收到通知
        viewModelScope.launch { push.register() }
    }

    fun refresh() {
        viewModelScope.launch {
            _state.value = try {
                UiState.Content(api.conversations())
            } catch (e: Exception) {
                UiState.Error(e.message ?: "error")
            }
        }
    }
}

@Composable
fun ChatsScreen(
    onOpenChat: (Long) -> Unit,
    onNewGroup: () -> Unit,
    onAccount: () -> Unit,
    onTab: (String) -> Unit,
    viewModel: ChatsViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val conversations = (state as? UiState.Content)?.data.orEmpty()

    // 会话列表的本地搜索：标题 + 最后一条消息预览
    var searching by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }

    // Android 13+ 要运行时授权才弹得出通知；放在登录后的第一屏要，别在冷启动就打扰人
    val context = LocalContext.current
    val askNotifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { }
    LaunchedEffect(Unit) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && !Notifications.canPost(context)) {
            askNotifications.launch(android.Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    ScreenRoot {
        BrandBar(
            onSubscribe = onAccount,
            onAccount = onAccount,
        )
        ImboTabs(
            active = "chats",
            chatsUnread = conversations.sumOf { it.pending },
            sessionsUnread = 0,
            onSelect = { if (it != "chats") onTab(it) },
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 8.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.End,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            GhostButton(
                text = if (searching) stringResource(R.string.cancel) else stringResource(R.string.search),
                onClick = {
                    searching = !searching
                    if (!searching) query = ""
                },
            )
            Spacer(Modifier.width(4.dp))
            GhostButton(text = stringResource(R.string.new_group), onClick = onNewGroup)
        }
        if (searching) {
            // 本地过滤：标题和最后一条消息预览都参与匹配，够日常找会话用
            InsetField(
                label = stringResource(R.string.search_chats_label),
                value = query,
                placeholder = stringResource(R.string.search_chats_placeholder),
                onValueChange = { query = it },
                modifier = Modifier.padding(horizontal = ImboDimen.Gutter, vertical = 4.dp),
            )
        }
        when (val s = state) {
            is UiState.Loading -> LoadingBlock()
            is UiState.Error -> ErrorBlock(s.message) { viewModel.refresh() }
            is UiState.Content -> {
                val visible = if (query.isBlank()) {
                    s.data
                } else {
                    s.data.filter { c ->
                        c.title.contains(query, ignoreCase = true) ||
                            c.lastMessage?.preview?.contains(query, ignoreCase = true) == true
                    }
                }
                if (s.data.isEmpty()) {
                    EmptyState(stringResource(R.string.empty_chats_title), stringResource(R.string.empty_chats_body))
                } else if (visible.isEmpty()) {
                    EmptyState(stringResource(R.string.search_empty_title), stringResource(R.string.search_empty_body, query))
                } else {
                    LazyColumn(modifier = Modifier.fillMaxSize()) {
                        items(visible, key = { it.id }) { conversation ->
                            ConversationRow(conversation) { onOpenChat(conversation.id) }
                        }
                    }
                }
            }
        }
    }
}

@HiltViewModel
class SessionsViewModel @Inject constructor(private val api: ImboApi) : ViewModel() {
    private val _state = MutableStateFlow<UiState<List<AgentSession>>>(UiState.Loading)
    val state: StateFlow<UiState<List<AgentSession>>> = _state.asStateFlow()

    init {
        refresh()
        viewModelScope.launch { api.changes.collect { refresh() } }
    }

    fun refresh() {
        viewModelScope.launch {
            _state.value = try {
                UiState.Content(api.agents())
            } catch (e: Exception) {
                UiState.Error(e.message ?: "error")
            }
        }
    }

    /** 改名只影响我这边的会话标题，写成功后重新拉列表。 */
    private val _notice = MutableStateFlow<String?>(null)
    val notice: StateFlow<String?> = _notice.asStateFlow()

    fun rename(agentId: String, title: String) {
        viewModelScope.launch {
            _notice.value = try {
                api.renameAgent(agentId, title)
                refresh()
                null
            } catch (e: Exception) {
                e.message ?: "error"
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun SessionsScreen(
    onOpenChat: (Long) -> Unit,
    onAccount: () -> Unit,
    onTab: (String) -> Unit,
    viewModel: SessionsViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val sessions = (state as? UiState.Content)?.data.orEmpty()
    val notice by viewModel.notice.collectAsStateWithLifecycle()
    // 长按会话改名，和联系人页的删除一样用长按承载次要动作
    var renameTarget by remember { mutableStateOf<AgentSession?>(null) }

    ScreenRoot {
        BrandBar(onSubscribe = onAccount, onAccount = onAccount)
        ImboTabs(
            active = "sessions",
            chatsUnread = 0,
            sessionsUnread = sessions.sumOf { it.pending },
            onSelect = { if (it != "sessions") onTab(it) },
        )
        when (val s = state) {
            is UiState.Loading -> LoadingBlock()
            is UiState.Error -> ErrorBlock(s.message) { viewModel.refresh() }
            is UiState.Content -> {
                if (s.data.isEmpty()) {
                    EmptyState(stringResource(R.string.empty_sessions_title), stringResource(R.string.empty_sessions_body))
                } else {
                    LazyColumn(modifier = Modifier.fillMaxSize().padding(horizontal = 8.dp)) {
                        items(s.data, key = { it.id }) { session ->
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .combinedClickable(
                                        onClick = {},
                                        onLongClick = { renameTarget = session },
                                    )
                                    .padding(vertical = 8.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Avatar(letter = session.title.take(1).uppercase())
                                Spacer(Modifier.height(0.dp))
                                Column(modifier = Modifier.weight(1f).padding(start = 12.dp)) {
                                    Text(
                                        text = session.title,
                                        color = ImboColor.OnBackground,
                                        fontFamily = ImboMono,
                                        fontSize = 14.sp,
                                    )
                                    Text(
                                        // 工具 + 所在电脑 + 在线状态：多台电脑时能分清是哪一个会话；
                                        // 离线时补一句「x 分钟前在线」，比光写 offline 有用
                                        text = listOfNotNull(
                                            session.tool,
                                            session.deviceName?.takeIf { it.isNotBlank() },
                                            if (session.online) {
                                                stringResource(R.string.session_online)
                                            } else {
                                                session.lastOnlineAt
                                                    ?.let { stringResource(R.string.session_last_seen, relativeTime(it)) }
                                                    ?.takeIf { it.isNotBlank() }
                                                    ?: stringResource(R.string.session_offline)
                                            },
                                        ).joinToString(" · "),
                                        color = ImboColor.Tertiary,
                                        fontFamily = ImboMono,
                                        fontSize = 12.sp,
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    renameTarget?.let { target ->
        ImboTextFieldDialog(
            title = stringResource(R.string.rename_session_title),
            label = stringResource(R.string.session_name_label),
            initial = target.title,
            confirmLabel = stringResource(R.string.save),
            onConfirm = { title ->
                renameTarget = null
                viewModel.rename(target.id, title)
            },
            onDismiss = { renameTarget = null },
        )
    }
    notice?.let { message ->
        Text(
            text = localizedServerError(message) ?: message,
            color = ImboColor.Error,
            fontFamily = ImboMono,
            fontSize = 12.sp,
            modifier = Modifier.padding(horizontal = ImboDimen.Gutter, vertical = 4.dp),
        )
    }
} 

@HiltViewModel
class ContactsViewModel @Inject constructor(private val api: ImboApi) : ViewModel() {
    private val _contacts = MutableStateFlow<UiState<List<Contact>>>(UiState.Loading)
    val contacts: StateFlow<UiState<List<Contact>>> = _contacts.asStateFlow()

    private val _requests = MutableStateFlow<List<ContactRequest>>(emptyList())
    val requests: StateFlow<List<ContactRequest>> = _requests.asStateFlow()

    init {
        refresh()
        viewModelScope.launch { api.changes.collect { refresh() } }
    }

    fun refresh() {
        viewModelScope.launch {
            _contacts.value = try {
                UiState.Content(api.contacts())
            } catch (e: Exception) {
                UiState.Error(e.message ?: "error")
            }
            _requests.value = try { api.requests() } catch (e: Exception) { emptyList() }
        }
    }

    fun accept(id: String) {
        viewModelScope.launch {
            runCatching { api.acceptRequest(id) }
            refresh()
        }
    }

    fun decline(id: String) {
        viewModelScope.launch {
            runCatching { api.declineRequest(id) }
            refresh()
        }
    }

    /** 解除联系人关系，历史消息仍在（服务端只删 contacts 行）。 */
    private val _notice = MutableStateFlow<String?>(null)
    val notice: StateFlow<String?> = _notice.asStateFlow()

    fun remove(id: String) {
        viewModelScope.launch {
            _notice.value = try {
                api.removeContact(id)
                refresh()
                null
            } catch (e: Exception) {
                e.message ?: "error"
            }
        }
    }

    /**
     * 点联系人直接进对应会话；还没聊过的人给一句提示（服务端要发第一条消息才会建会话）。
     * 群也躺在联系人列表里，按群 id 匹配。
     */
    fun openContact(contact: Contact, onOpenChat: (Long) -> Unit) {
        viewModelScope.launch {
            val all = runCatching { api.conversations() }.getOrDefault(emptyList())
            val target = all.firstOrNull {
                it.peer?.id == contact.id || it.group?.id == contact.id
            }
            if (target != null) {
                onOpenChat(target.id)
            } else {
                _notice.value = noChatYet
            }
        }
    }

    /** 由界面注入的本地化提示，ViewModel 里拿不到 stringResource。 */
    var noChatYet: String = ""
}

@OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)
@Composable
fun ContactsScreen(
    onAddContact: () -> Unit,
    onOpenChat: (Long) -> Unit,
    onAccount: () -> Unit,
    onTab: (String) -> Unit,
    viewModel: ContactsViewModel = hiltViewModel(),
) {
    val contactsState by viewModel.contacts.collectAsStateWithLifecycle()
    val requests by viewModel.requests.collectAsStateWithLifecycle()
    val notice by viewModel.notice.collectAsStateWithLifecycle()
    val contacts = (contactsState as? UiState.Content)?.data.orEmpty()
    // 长按联系人弹出解除确认；联系人关系删掉后历史消息仍在
    var removeTarget by remember { mutableStateOf<dev.imbo.android.data.Contact?>(null) }
    // 点还没聊过的联系人时用的提示（ViewModel 里拿不到 stringResource）
    viewModel.noChatYet = stringResource(R.string.contact_no_chat_yet)

    ScreenRoot {
        BrandBar(onSubscribe = onAccount, onAccount = onAccount)
        ImboTabs(active = "contacts", chatsUnread = 0, sessionsUnread = 0, onSelect = { if (it != "contacts") onTab(it) })
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 8.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.End,
        ) {
            GhostButton(text = stringResource(R.string.add_contact), onClick = onAddContact)
        }
        when (val s = contactsState) {
            is UiState.Loading -> LoadingBlock()
            is UiState.Error -> ErrorBlock(s.message) { viewModel.refresh() }
            is UiState.Content -> {
                LazyColumn(modifier = Modifier.fillMaxSize()) {
                    if (requests.isNotEmpty()) {
                        item(key = "requests-label") { SectionLabel(stringResource(R.string.requests)) }
                        items(requests, key = { "request:" + it.id }) { request ->
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .padding(horizontal = 8.dp, vertical = 8.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Avatar(letter = request.name.take(1).uppercase())
                                Column(modifier = Modifier.weight(1f).padding(start = 12.dp)) {
                                    Text(request.name, color = ImboColor.OnBackground, fontFamily = ImboMono, fontSize = 14.sp)
                                    Text(
                                        text = when {
                                            request.kind == "group" ->
                                                stringResource(R.string.group_invite_request, request.groupName ?: request.name)
                                            !request.note.isNullOrBlank() -> request.note!!
                                            else -> stringResource(R.string.request_pending)
                                        },
                                        color = ImboColor.Tertiary,
                                        fontFamily = ImboMono,
                                        fontSize = 12.sp,
                                    )
                                }
                                GhostButton(text = stringResource(R.string.accept), onClick = { viewModel.accept(request.id) })
                                Spacer(Modifier.padding(horizontal = 4.dp))
                                GhostButton(text = stringResource(R.string.decline), onClick = { viewModel.decline(request.id) })
                            }
                        }
                    }
                    if (s.data.isEmpty()) {
                        item(key = "empty") {
                            EmptyState(stringResource(R.string.empty_chats_title), stringResource(R.string.empty_chats_body))
                        }
                    } else {
                        items(s.data, key = { "contact:" + it.id }) { contact ->
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .combinedClickable(
                                        onClick = { viewModel.openContact(contact, onOpenChat) },
                                        onLongClick = { removeTarget = contact },
                                    )
                                    .padding(horizontal = 8.dp, vertical = 8.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Avatar(
                                    letter = contact.name.take(1).uppercase(),
                                    group = contact.subtitle?.contains("members") == true,
                                )
                                Column(modifier = Modifier.weight(1f).padding(start = 12.dp)) {
                                    Text(contact.name, color = ImboColor.OnBackground, fontFamily = ImboMono, fontSize = 14.sp)
                                    contact.subtitle?.let {
                                        Text(it, color = ImboColor.Tertiary, fontFamily = ImboMono, fontSize = 12.sp)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    removeTarget?.let { target ->
        dev.imbo.android.ui.components.ImboDialog(
            title = stringResource(R.string.delete_contact_title, target.name),
            body = stringResource(R.string.delete_contact_body),
            confirmLabel = stringResource(R.string.delete_contact_action),
            onConfirm = {
                removeTarget = null
                viewModel.remove(target.id)
            },
            onDismiss = { removeTarget = null },
            destructive = true,
        )
    }
    notice?.let { message ->
        Text(
            text = localizedServerError(message) ?: message,
            color = ImboColor.Error,
            fontFamily = ImboMono,
            fontSize = 12.sp,
            modifier = Modifier.padding(horizontal = ImboDimen.Gutter, vertical = 4.dp),
        )
    }
}

@Composable
fun LoadingBlock() {
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Text(
            text = stringResource(R.string.loading),
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
        )
    }
}

@Composable
fun ErrorBlock(message: String, onRetry: () -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(horizontal = ImboDimen.Gutter),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = stringResource(R.string.error_generic),
            color = ImboColor.OnBackground,
            fontFamily = ImboMono,
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold,
        )
        Spacer(Modifier.height(4.dp))
        // 服务端的 detail 是英文，已知的换成中文；未知的原样显示，别把失败原因吞掉
        Text(
            text = localizedServerError(message) ?: message,
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
        )
        Spacer(Modifier.height(16.dp))
        GhostButton(text = stringResource(R.string.retry), onClick = onRetry)
    }
}
