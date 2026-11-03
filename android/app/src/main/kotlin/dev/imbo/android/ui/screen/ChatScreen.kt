package dev.imbo.android.ui.screen

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material3.Icon
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import kotlinx.coroutines.launch
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.imbo.android.R
import dev.imbo.android.data.Attachment
import dev.imbo.android.data.ChangeArea
import dev.imbo.android.data.Contact
import dev.imbo.android.data.Conversation
import dev.imbo.android.data.Delivery
import dev.imbo.android.data.ImboApi
import dev.imbo.android.data.Message
import dev.imbo.android.data.UiState
import dev.imbo.android.ui.components.AttachmentPickerDialog
import dev.imbo.android.ui.components.ChatActionsDialog
import dev.imbo.android.ui.components.ChatHeader
import dev.imbo.android.ui.components.Composer
import dev.imbo.android.ui.components.DayDivider
import dev.imbo.android.ui.components.EmptyState
import dev.imbo.android.ui.components.ForwardDialog
import dev.imbo.android.ui.components.Hairline
import dev.imbo.android.ui.components.ImageViewer
import dev.imbo.android.ui.components.ImboDialog
import dev.imbo.android.ui.components.InsetField
import dev.imbo.android.ui.components.MessageActionsDialog
import dev.imbo.android.ui.components.MessageRow
import dev.imbo.android.ui.components.localizedServerError
import dev.imbo.android.ui.components.MentionPickerDialog
import dev.imbo.android.ui.components.ReportDialog
import dev.imbo.android.ui.components.ScreenRoot
import dev.imbo.android.ui.components.UnreadDivider
import dev.imbo.android.ui.components.ShareImage
import dev.imbo.android.ui.components.dayLabel
import dev.imbo.android.ui.theme.ImboColor
import dev.imbo.android.ui.theme.ImboDimen
import dev.imbo.android.ui.theme.ImboMono
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch
import java.util.UUID
import javax.inject.Inject

private const val PAGE_SIZE = 50

@HiltViewModel
class ChatViewModel @Inject constructor(
    private val api: ImboApi,
    private val drafts: dev.imbo.android.data.DraftStorage,
) : ViewModel() {

    private val _state = MutableStateFlow<UiState<List<Message>>>(UiState.Loading)
    val state: StateFlow<UiState<List<Message>>> = _state.asStateFlow()

    private val _conversation = MutableStateFlow<Conversation?>(null)
    val conversation: StateFlow<Conversation?> = _conversation.asStateFlow()

    /** 打开会话时的未读数；用来在第一条未读消息上方画「以下为新消息」。标已读后不再变。 */
    private var dividerSeedFor: Long? = null
    private val _unreadAtOpen = MutableStateFlow(0)
    val unreadAtOpen: StateFlow<Int> = _unreadAtOpen.asStateFlow()

    /** 是否还有更早的消息可加载；到顶后不再请求。 */
    private val _hasMore = MutableStateFlow(false)
    val hasMore: StateFlow<Boolean> = _hasMore.asStateFlow()

    /** 是否正在加载更早的消息，聊天页据此在顶部显示一行提示。 */
    private val _loadingMore = MutableStateFlow(false)
    val loadingMore: StateFlow<Boolean> = _loadingMore.asStateFlow()

    /** 举报/拉黑后的确认文案，显示在输入框上方一行。 */
    private val _notice = MutableStateFlow<String?>(null)
    val notice: StateFlow<String?> = _notice.asStateFlow()

    /** 群成员，给输入 @ 时的选择框用；单聊为空。 */
    private val _members = MutableStateFlow<List<Contact>>(emptyList())
    val members: StateFlow<List<Contact>> = _members.asStateFlow()

    /** 有人刚接受邀请、或成员变动后，@ 列表要跟着更新，不然只看到进群时的那几个人。 */
    fun refreshMembers() {
        val groupId = _conversation.value?.group?.id ?: return
        viewModelScope.launch {
            val latest = runCatching { api.groupDetail(groupId).members }.getOrDefault(emptyList())
            if (latest.isNotEmpty()) _members.value = latest
        }
    }

    private var conversationId: Long = 0
    private var subscribed = false

    fun load(id: Long) {
        if (conversationId != id) {
            conversationId = id
            _state.value = UiState.Loading
            _hasMore.value = false
            _loadingMore.value = false
        }
        viewModelScope.launch {
            val conversation = runCatching { api.conversation(id) }.getOrNull()
            _conversation.value = conversation
            // 标已读之前先记未读数：聊天页据此在第一条未读消息上方画分隔线。
            // 只记第一次：标已读会触发一次变更回调，再记就把未读数覆盖成 0 了
            if (dividerSeedFor != id) {
                dividerSeedFor = id
                // 缓存里的 pending 可能过期（消息进来时没刷新列表），这里以一次实时列表为准
                val pendingNow = runCatching {
                    api.conversations().firstOrNull { it.id == id }?.pending
                }.getOrNull() ?: conversation?.pending ?: 0
                _unreadAtOpen.value = pendingNow
            }
            // agent 会话按会话 id 标已读；单聊和群等消息加载完再标（服务端要 up_to）
            val agentId = conversation?.agentId
            if (agentId != null) {
                runCatching { api.markRead(agentId) }
            }
            // 群聊才需要成员列表（@ 提及）；拿不到就留空，输入 @ 时只是没有候选
            if (conversation?.isGroup == true) {
                val groupId = conversation.group?.id
                if (groupId != null) {
                    _members.value = runCatching { api.groupDetail(groupId).members }.getOrDefault(emptyList())
                }
            }
            refresh()
            // 消息到手后按「最后一条」标已读：服务端要求 up_to，不带的话一直是未读。
            // 这里直接取一页，别依赖 refresh()——它是异步的，读 _state 时可能还是空的
            if (id > 0) {
                val newest = runCatching { api.messages(id, limit = PAGE_SIZE).messages.lastOrNull()?.id }
                    .getOrNull()
                if (newest != null && newest > 0) {
                    runCatching { api.markConversationRead(id, newest) }
                }
            }
        }
        if (!subscribed) {
            subscribed = true
            viewModelScope.launch {
                api.changes.collect { area ->
                    // 群管理改的是会话本身（群名、成员数），标题也要跟着变
                    if (area == ChangeArea.Conversations || area == ChangeArea.All) refreshConversation()
                    refresh()
                }
            }
        }
    }

    /** 返回聊天页时群名可能已经在群信息页改过，不重新拉会话列表就会显示旧标题。 */
    private fun refreshConversation() {
        if (conversationId == 0L) return
        viewModelScope.launch {
            runCatching { api.conversation(conversationId) }.getOrNull()?.let { _conversation.value = it }
        }
    }

    fun refresh() {
        if (conversationId == 0L) return
        val id = conversationId
        // 全量刷新重置分页状态：回到最新一页，之后需要重新向上翻
        _hasMore.value = false
        _loadingMore.value = false
        viewModelScope.launch {
            _state.value = try {
                val page = api.messages(id, limit = PAGE_SIZE)
                if (id == conversationId) _hasMore.value = page.hasMore
                UiState.Content(page.messages)
            } catch (e: Exception) {
                UiState.Error(e.message ?: "error")
            }
            resolveDownloadableAttachments()
        }
    }

    /**
     * 服务端只给文件 id，图片要下载到本机才显示得出来。
     * 只处理还缺本地路径的图片附件，下载失败就保持文件名展示。
     */
    private suspend fun resolveDownloadableAttachments() {
        val current = (_state.value as? UiState.Content)?.data ?: return
        val pending = current.flatMap { it.attachments }
            .filter { it.localUri == null && it.mimeType.startsWith("image/") }
        if (pending.isEmpty()) return
        val resolved = pending.associate { att ->
            att.id to runCatching { api.downloadAttachment(att.id, att.name) }.getOrNull()
        }
        val latest = (_state.value as? UiState.Content)?.data ?: return
        _state.value = UiState.Content(
            latest.map { message ->
                message.copy(
                    attachments = message.attachments.map { att ->
                        val path = resolved[att.id]
                        if (att.localUri == null && path != null) att.copy(localUri = path) else att
                    },
                )
            },
        )
    }

    /**
     * 加载更早的一页，追加到列表最前面。
     * 已经存在的消息按 id 去重，避免刷新与新拉取交错时出现重复气泡；
     * hasMore 为 false 时直接返回，不再打服务端。
     */
    fun loadOlder() {
        if (_loadingMore.value || !_hasMore.value) return
        val current = (_state.value as? UiState.Content)?.data ?: return
        val oldest = current.firstOrNull { it.id > 0 }?.id ?: return
        val id = conversationId
        _loadingMore.value = true
        viewModelScope.launch {
            try {
                val page = api.messages(id, before = oldest, limit = PAGE_SIZE)
                if (id == conversationId) {
                    // 以最新状态为准，和刷新并发时不把旧列表拼回去
                    val latest = (_state.value as? UiState.Content)?.data ?: current
                    val known = latest.mapTo(mutableSetOf()) { it.id }
                    val older = page.messages.filter { known.add(it.id) }
                    if (older.isNotEmpty()) _state.value = UiState.Content(older + latest)
                    _hasMore.value = page.hasMore
                }
            } catch (e: Exception) {
                _notice.value = e.message
            } finally {
                _loadingMore.value = false
            }
        }
    }

    // ---------------------------------------------------------------- 草稿

    /** 草稿按对话 id 存：退出对话再回来还在，切换对话也不会串稿。 */
    fun draftFor(id: Long): String = drafts.read(id)

    fun saveDraft(text: String) {
        if (conversationId == 0L) return
        drafts.write(conversationId, text)
    }

    fun clearDraft() {
        if (conversationId == 0L) return
        drafts.clear(conversationId)
    }

    /** 乐观回显：立即上屏 sending，失败留在原位转 failed。 */
    fun send(body: String, attachments: List<Attachment> = emptyList()) {
        if (body.isBlank() && attachments.isEmpty()) return
        val clientId = UUID.randomUUID().toString()
        appendOptimistic(
            Message(
                id = -System.currentTimeMillis(),
                conversationId = conversationId,
                body = body,
                senderName = "alice",
                mine = true,
                createdAt = System.currentTimeMillis(),
                clientId = clientId,
                delivery = Delivery.Sending,
                attachments = attachments,
            ),
        )
        viewModelScope.launch {
            settle(clientId, runCatching { api.sendMessage(conversationId, body, clientId, attachments) })
        }
    }

    /**
     * 选好附件后：先上传拿到服务端文件 id，再发送一条带附件的消息。
     * 上传和发送期间气泡已经带着本地附件上屏（sending），图片用本地 uri 预览；
     * 任何一步失败都留在原地标 failed，重试会重新上传。
     */
    fun sendAttachment(localUri: String, name: String, mimeType: String, sizeBytes: Long = 0) {
        val clientId = UUID.randomUUID().toString()
        appendOptimistic(
            Message(
                id = -System.currentTimeMillis(),
                conversationId = conversationId,
                body = "",
                senderName = "alice",
                mine = true,
                createdAt = System.currentTimeMillis(),
                clientId = clientId,
                delivery = Delivery.Sending,
                attachments = listOf(
                    Attachment(
                        id = "local-$clientId",
                        name = name,
                        mimeType = mimeType,
                        localUri = localUri,
                        sizeBytes = sizeBytes,
                    ),
                ),
            ),
        )
        viewModelScope.launch {
            val result = runCatching {
                val uploaded = api.uploadAttachment(localUri, name, mimeType)
                val attachment = uploaded.copy(
                    localUri = localUri,
                    sizeBytes = uploaded.sizeBytes.takeIf { it > 0 } ?: sizeBytes,
                )
                // 上传完成后先换成真实附件 id，气泡里的图片仍指向本地 uri
                replaceAttachment(clientId, attachment)
                api.sendMessage(conversationId, "", clientId, listOf(attachment))
            }
            result.exceptionOrNull()?.message?.let { _notice.value = it }
            settle(clientId, result)
        }
    }

    private fun appendOptimistic(message: Message) {
        val current = (_state.value as? UiState.Content)?.data.orEmpty()
        _state.value = UiState.Content(current + message)
    }

    private fun replaceAttachment(clientId: String, attachment: Attachment) {
        val list = (_state.value as? UiState.Content)?.data.orEmpty()
        _state.value = UiState.Content(
            list.map { if (it.clientId == clientId) it.copy(attachments = listOf(attachment)) else it },
        )
    }

    private fun settle(clientId: String, result: Result<Message>) {
        val list = (_state.value as? UiState.Content)?.data.orEmpty()
        // 失败必须让人看见：气泡留在原位、转 failed，并把原因写到提示行（DATA-004）
        result.exceptionOrNull()?.let { _notice.value = it.message ?: "发送失败" }
        _state.value = UiState.Content(
            list.map { msg ->
                if (msg.clientId != clientId) {
                    msg
                } else {
                    result.fold(
                        onSuccess = { server -> server.copy(clientId = clientId, delivery = Delivery.Sent) },
                        onFailure = { msg.copy(delivery = Delivery.Failed) },
                    )
                }
            },
        )
    }

    fun retry(message: Message) {
        _state.value = UiState.Content((_state.value as? UiState.Content)?.data.orEmpty().filterNot { it.clientId == message.clientId })
        val pending = message.attachments.firstOrNull { it.localUri != null }
        if (pending != null) {
            val localUri = pending.localUri ?: return
            sendAttachment(localUri, pending.name, pending.mimeType, pending.sizeBytes)
        } else {
            send(message.body, message.attachments)
        }
    }

    fun report(targetId: String, reason: String, block: Boolean, onBlocked: () -> Unit) {
        viewModelScope.launch {
            runCatching { api.report(targetId, reason) }
                .onFailure { _notice.value = it.message }
            if (block) {
                val result = runCatching { api.block(targetId) }
                if (result.isSuccess) onBlocked() else _notice.value = result.exceptionOrNull()?.message
            }
        }
    }

    fun block(targetId: String, onBlocked: () -> Unit) {
        viewModelScope.launch {
            val result = runCatching { api.block(targetId) }
            if (result.isSuccess) onBlocked() else _notice.value = result.exceptionOrNull()?.message
        }
    }

    fun clearNotice() {
        _notice.value = null
    }

    /** 一次性提示，几秒后自动消失，不占用户注意力。 */
    fun note(text: String) {
        _notice.value = text
        viewModelScope.launch {
            kotlinx.coroutines.delay(2500)
            if (_notice.value == text) _notice.value = null
        }
    }

    /** 转发时可选的会话（不含当前会话本身）。 */
    private val _forwardTargets = MutableStateFlow<List<Conversation>>(emptyList())
    val forwardTargets: StateFlow<List<Conversation>> = _forwardTargets.asStateFlow()

    fun loadForwardTargets() {
        viewModelScope.launch {
            val all = runCatching { api.conversations() }.getOrDefault(emptyList())
            _forwardTargets.value = all.filterNot { it.id == conversationId }
        }
    }

    /**
     * 把一条消息原样转发到另一个会话：正文和附件都照搬。
     * 附件 id 是服务端文件 id，服务端认得，不需要重新上传。
     */
    fun forward(message: Message, targetId: Long) {
        viewModelScope.launch {
            try {
                api.sendMessage(
                    conversationId = targetId,
                    body = message.body,
                    clientId = UUID.randomUUID().toString(),
                    attachments = message.attachments,
                )
                note(forwardedText)
            } catch (e: Exception) {
                _notice.value = e.message ?: "error"
            }
        }
    }

    /** 由界面注入的本地化「已转发」文案，ViewModel 里拿不到 stringResource。 */
    var forwardedText: String = ""
}

@Composable
fun ChatScreen(
    conversationId: Long,
    onBack: () -> Unit,
    onOpenGroupInfo: () -> Unit,
    viewModel: ChatViewModel = hiltViewModel(),
) {
    LaunchedEffect(conversationId) { viewModel.load(conversationId) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val conversation by viewModel.conversation.collectAsStateWithLifecycle()
    val notice by viewModel.notice.collectAsStateWithLifecycle()
    val hasMore by viewModel.hasMore.collectAsStateWithLifecycle()
    val loadingMore by viewModel.loadingMore.collectAsStateWithLifecycle()
    // 草稿按对话从 SavedStateHandle 恢复，离开再回来文字还在
    var draft by remember(conversationId) { mutableStateOf(viewModel.draftFor(conversationId)) }
    var searching by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }
    var showMenu by remember { mutableStateOf(false) }
    var showReport by remember { mutableStateOf(false) }
    var showBlock by remember { mutableStateOf(false) }
    var showAttach by remember { mutableStateOf(false) }
    var reported by remember { mutableStateOf(false) }
    var actionMessage by remember { mutableStateOf<Message?>(null) }
    // 转发：长按消息选「转发」后弹目标会话列表
    var forwardMessage by remember { mutableStateOf<Message?>(null) }
    // 点图片看大图
    var viewerImage by remember { mutableStateOf<Attachment?>(null) }
    val forwardTargets by viewModel.forwardTargets.collectAsStateWithLifecycle()
    val forwardedLabel = stringResource(R.string.forward_done)
    // 输入 @ 时弹出的成员选择框；只在群聊里有候选
    val members by viewModel.members.collectAsStateWithLifecycle()
    val unreadAtOpen by viewModel.unreadAtOpen.collectAsStateWithLifecycle()
    var mentionPicker by remember { mutableStateOf(false) }
    // 多选分享：选中的消息 id（服务端没有删除接口，这里只用于分享图片）
    var selecting by remember { mutableStateOf(false) }
    val picked = remember { mutableStateListOf<Long>() }
    val togglePick: (Long) -> Unit = { id ->
        if (picked.contains(id)) picked.remove(id) else picked.add(id)
    }
    // 加载更早消息前记下锚点，数据回来后按 key 找回位置，避免列表跳动
    var scrollAnchor by remember { mutableStateOf<ScrollAnchor?>(null) }
    val listState = rememberLazyListState()
    val messages = (state as? UiState.Content)?.data.orEmpty()
    // 同名会话（同工具多开）在群里各说各话时，靠短 id 区分
    val duplicatedSenders = messages.filterNot { it.mine }.groupBy { it.senderName }.filterValues { it.size > 1 }.keys
    val clipboard = LocalClipboardManager.current

    val visible = if (query.isBlank()) {
        messages
    } else {
        messages.filter { it.body.contains(query, ignoreCase = true) }
    }
    val dayGroups = visible.groupBy { dayLabel(it.createdAt) }
    // 未读分隔线：只在非搜索态、且未读数落在当前这一页里时显示
    val unreadDividerId = if (query.isBlank() && unreadAtOpen in 1..visible.size) {
        visible[visible.size - unreadAtOpen].id
    } else {
        null
    }
    val itemKeys = dayGroups.flatMap { (day, dayMessages) ->
        listOf("day-$day") + dayMessages.map { it.clientId ?: it.id.toString() }
    }

    // 选中的图片/文件统一走同一个入口：先上传，再发一条带附件的消息。
    val context = LocalContext.current
    val fallbackName = stringResource(R.string.attach_default_name)
    val onPicked: (Uri?) -> Unit = { uri ->
        if (uri != null) {
            val picked = queryAttachment(context, uri, fallbackName)
            viewModel.sendAttachment(uri.toString(), picked.name, picked.mimeType, picked.sizeBytes)
        }
    }
    val photoPicker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { onPicked(it) }
    val filePicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { onPicked(it) }

    // 相机：先占一个缓存文件，把 FileProvider 的 content:// 交给系统相机，
    // 拍完按同一个 onPicked 上传；失败就把占位文件删掉。
    val cameraUri = remember { mutableStateOf<Uri?>(null) }
    val cameraLauncher = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { ok ->
        val uri = cameraUri.value
        cameraUri.value = null
        if (ok && uri != null) onPicked(uri)
    }
    val launchCamera = {
        val dir = java.io.File(context.cacheDir, "camera").apply { mkdirs() }
        val file = java.io.File(dir, "imbo-${System.currentTimeMillis()}.jpg")
        val uri = androidx.core.content.FileProvider.getUriForFile(
            context,
            "${context.packageName}.fileprovider",
            file,
        )
        cameraUri.value = uri
        cameraLauncher.launch(uri)
    }

    // 首条可见时提前一屏拉更早的消息；搜索态列表是过滤后的，不分页
    val canLoadEarlier = hasMore && !loadingMore && query.isBlank() && messages.isNotEmpty()
    val canLoadEarlierNow by rememberUpdatedState(canLoadEarlier)
    LaunchedEffect(listState) {
        snapshotFlow { listState.firstVisibleItemIndex }
            .distinctUntilChanged()
            .collect { index ->
                if (index <= 1 && canLoadEarlierNow) {
                    val anchorKey = listState.layoutInfo.visibleItemsInfo.firstOrNull()?.key as? String
                    if (anchorKey != null) {
                        scrollAnchor = ScrollAnchor(anchorKey, listState.firstVisibleItemScrollOffset)
                        viewModel.loadOlder()
                        // 条件不满足时 loadOlder 会直接返回，别留下无用的锚点
                        if (!viewModel.loadingMore.value) scrollAnchor = null
                    }
                }
            }
    }

    // 更早的消息插入到前面后，滚回原来的锚点条目，阅读位置不跳
    LaunchedEffect(loadingMore, messages.size) {
        val anchor = scrollAnchor ?: return@LaunchedEffect
        if (loadingMore) return@LaunchedEffect
        val index = itemKeys.indexOf(anchor.key)
        if (index >= 0) listState.scrollToItem(index, anchor.offset)
        scrollAnchor = null
    }

    // 只有末尾追加新消息时才滚到底；向上加载历史不触发，避免把用户甩回最新一条
    val lastMessageKey = messages.lastOrNull()?.let { it.clientId ?: it.id.toString() }
    LaunchedEffect(lastMessageKey) {
        if (lastMessageKey != null && query.isBlank()) {
            listState.animateScrollToItem(itemKeys.lastIndex)
        }
    }

    val group = conversation?.isGroup == true
    val isAgent = conversation?.peer?.agent == true
    val title = conversation?.title?.ifBlank { "chat" } ?: "chat"
    // 群头像存在会话的 group 上；单聊暂时没有对端头像字段可用
    val avatarUrl = conversation?.group?.avatarUrl ?: conversation?.peer?.avatarUrl
    val subtitle = when {
        isAgent -> stringResource(R.string.ai_agent_hint, conversation?.peer?.subtitle ?: "agent")
        // 群成员数来自数据里的副标题（"4 members"），取数字后按当前语言重新渲染，
        // 否则中文界面里会露出一句英文
        group -> conversation?.group?.subtitle
            ?.filter { it.isDigit() }
            ?.toIntOrNull()
            ?.let { stringResource(R.string.members_count, it) }
            ?: conversation?.group?.subtitle.orEmpty()
        conversation?.peer?.online == true -> stringResource(R.string.online)
        else -> conversation?.peer?.subtitle.orEmpty()
    }
    val targetId = conversation?.peer?.id ?: conversation?.group?.id.orEmpty()

    ScreenRoot {
        ChatHeader(
            title = title,
            subtitle = subtitle,
            onBack = onBack,
            showAvatar = true,
            group = group,
            agent = isAgent,
            avatarUrl = avatarUrl,
            onSearch = { searching = !searching; if (!searching) query = "" },
            onMore = { showMenu = true },
        )
        if (searching) {
            InsetField(
                label = stringResource(R.string.chat_search_placeholder),
                value = query,
                placeholder = "",
                onValueChange = { query = it },
                modifier = Modifier.padding(horizontal = ImboDimen.Gutter, vertical = 8.dp),
            )
        }
        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
            when (val s = state) {
                is UiState.Loading -> LoadingBlock()
                is UiState.Error -> ErrorBlock(s.message) { viewModel.refresh() }
                is UiState.Content -> {
                    if (visible.isEmpty()) {
                        // 三种空：搜索没命中、群还没消息、单聊还没消息
                        when {
                            query.isNotBlank() -> EmptyState(
                                stringResource(R.string.chat_search_empty),
                                stringResource(R.string.chat_search_empty_body, query),
                            )
                            group -> EmptyState(
                                stringResource(R.string.chat_empty_group_title),
                                stringResource(R.string.chat_empty_group_body),
                            )
                            else -> EmptyState(
                                stringResource(R.string.chat_empty_title),
                                stringResource(R.string.chat_empty_body),
                            )
                        }
                    } else {
                        LazyColumn(state = listState, modifier = Modifier.fillMaxSize()) {
                            if (loadingMore) {
                                item(key = "loading-earlier") {
                                    Text(
                                        text = stringResource(R.string.chat_loading_earlier),
                                        color = ImboColor.Tertiary,
                                        fontFamily = ImboMono,
                                        fontSize = 12.sp,
                                        modifier = Modifier
                                            .fillMaxWidth()
                                            .padding(horizontal = ImboDimen.Gutter, vertical = 8.dp),
                                    )
                                }
                            }
                            dayGroups.forEach { (day, dayMessages) ->
                                item(key = "day-$day") { DayDivider(day) }
                                items(dayMessages, key = { it.clientId ?: it.id.toString() }) { message ->
                                    Column(modifier = Modifier.fillMaxWidth()) {
                                        if (message.id == unreadDividerId) UnreadDivider()
                                    MessageRow(
                                        message = message,
                                        group = group,
                                        duplicatedSenders = duplicatedSenders,
                                        selected = selecting && picked.contains(message.id),
                                        onClick = {
                                            when {
                                                selecting -> togglePick(message.id)
                                                // 失败的气泡点一下就是重试
                                                message.delivery == Delivery.Failed -> viewModel.retry(message)
                                                else -> Unit
                                            }
                                        },
                                        onLongPress = { actionMessage = it },
                                        onOpenAttachment = { viewerImage = it },
                                    )
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // 往上翻历史时给一个回到底部的入口；已经在底部就不挡视线
            if (listState.canScrollForward) {
                val jumpScope = rememberCoroutineScope()
                Box(
                    modifier = Modifier
                        .align(Alignment.BottomEnd)
                        .padding(end = 16.dp, bottom = 16.dp)
                        .size(36.dp)
                        .clip(CircleShape)
                        .background(ImboColor.SecondaryContainer)
                        .border(1.dp, ImboColor.OutlineVariant, CircleShape)
                        .clickable {
                            jumpScope.launch {
                                val last = (listState.layoutInfo.totalItemsCount - 1).coerceAtLeast(0)
                                listState.animateScrollToItem(last)
                            }
                        },
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        Icons.Outlined.KeyboardArrowDown,
                        contentDescription = stringResource(R.string.chat_jump_to_latest),
                        tint = ImboColor.OnBackground,
                        modifier = Modifier.size(20.dp),
                    )
                }
            }
        }
        (notice ?: if (reported) stringResource(R.string.report_sent) else null)?.let { text ->
            Text(
                text = localizedServerError(text) ?: text,
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = ImboDimen.Gutter, vertical = 4.dp),
            )
        }
        Column(modifier = Modifier.imePadding()) {
            if (selecting) {
                // 多选分享：把选中的消息画成一张图交给系统分享
                val shareSelected: () -> Unit = {
                    val chosen = messages.filter { picked.contains(it.id) }
                    val intent = ShareImage.shareIntent(context, title, chosen)
                    if (intent != null) context.startActivity(Intent.createChooser(intent, null))
                    selecting = false
                    picked.clear()
                }
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = ImboDimen.Gutter, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    Text(
                        text = stringResource(R.string.share_selected_count, picked.size),
                        color = ImboColor.OnBackground,
                        fontFamily = ImboMono,
                        fontSize = 13.sp,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        text = stringResource(R.string.share_as_image),
                        color = if (picked.isEmpty()) ImboColor.Tertiary else ImboColor.OnBackground,
                        fontFamily = ImboMono,
                        fontSize = 13.sp,
                        modifier = Modifier.clickable(enabled = picked.isNotEmpty()) { shareSelected() },
                    )
                    Text(
                        text = stringResource(R.string.cancel),
                        color = ImboColor.Tertiary,
                        fontFamily = ImboMono,
                        fontSize = 13.sp,
                        modifier = Modifier.clickable {
                            selecting = false
                            picked.clear()
                        },
                    )
                }
                Hairline(vertical = false)
            }
            Composer(
                value = draft,
                onValueChange = {
                    // 刚打出 @ 就弹成员选择；顺便刷新一次成员，别人刚接受邀请也能出现在列表里
                    if (group && it.endsWith("@")) {
                        viewModel.refreshMembers()
                        mentionPicker = true
                    }
                    draft = it
                    viewModel.saveDraft(it)
                },
                onSend = {
                    viewModel.send(draft)
                    draft = ""
                    // 发送后清掉这个对话的草稿，下次进来输入框是空的
                    viewModel.clearDraft()
                },
                onAttach = { showAttach = true },
            )
        }
    }

    if (mentionPicker) {
        MentionPickerDialog(
            members = members,
            onPick = { member ->
                // 替换掉末尾那个 @，换成完整的「@名字 」
                draft = draft.trimEnd('@') + "@${member.name} "
                viewModel.saveDraft(draft)
                mentionPicker = false
            },
            onDismiss = { mentionPicker = false },
        )
    }

    if (showMenu) {
        ChatActionsDialog(
            agentName = conversation?.peer?.takeIf { it.agent }?.subtitle,
            onReport = { showMenu = false; showReport = true },
            onBlock = { showMenu = false; showBlock = true },
            onDismiss = { showMenu = false },
            onGroupInfo = if (group) {
                { showMenu = false; onOpenGroupInfo() }
            } else {
                null
            },
        )
    }
    if (showReport) {
        ReportDialog(
            onReport = { reason ->
                showReport = false
                viewModel.report(targetId, reason, block = false) {}
                reported = true
            },
            onReportAndBlock = { reason ->
                showReport = false
                viewModel.report(targetId, reason, block = true) {
                    reported = true
                    onBack()
                }
            },
            onDismiss = { showReport = false },
        )
    }
    if (showBlock) {
        ImboDialog(
            title = stringResource(R.string.block_action),
            body = stringResource(R.string.blocked_hint),
            confirmLabel = stringResource(R.string.block_action),
            onConfirm = {
                showBlock = false
                viewModel.block(targetId) { onBack() }
            },
            onDismiss = { showBlock = false },
            destructive = true,
        )
    }
    if (showAttach) {
        AttachmentPickerDialog(
            onCamera = {
                showAttach = false
                launchCamera()
            },
            onPhoto = {
                showAttach = false
                photoPicker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
            },
            onFile = {
                showAttach = false
                filePicker.launch(arrayOf("*/*"))
            },
            onDismiss = { showAttach = false },
        )
    }
    val noBodyText = stringResource(R.string.chat_no_body)
    actionMessage?.let { message ->
        MessageActionsDialog(
            onCopy = {
                // 图片/文件消息可能没有正文，复制时用占位文案兜底
                clipboard.setText(AnnotatedString(message.body.ifBlank { noBodyText }))
                actionMessage = null
            },
            onQuote = {
                val quoted = quoteDraft(message.body, draft, noBodyText)
                draft = quoted
                viewModel.saveDraft(quoted)
                actionMessage = null
            },
            onSelect = {
                // 进入多选：先选上长按的这条，再点别的消息加减
                selecting = true
                if (!picked.contains(message.id)) picked.add(message.id)
                actionMessage = null
            },
            onForward = {
                forwardMessage = message
                viewModel.loadForwardTargets()
                actionMessage = null
            },
            onDismiss = { actionMessage = null },
        )
    }

    viewerImage?.let { image ->
        ImageViewer(image) { viewerImage = null }
    }

    forwardMessage?.let { message ->
        ForwardDialog(
            targets = forwardTargets,
            onPick = { targetId ->
                viewModel.forwardedText = forwardedLabel
                viewModel.forward(message, targetId)
                forwardMessage = null
            },
            onDismiss = { forwardMessage = null },
        )
    }
}

/** 向上翻页时记住的锚点：第一个可见条目的 key 和像素偏移。 */
private data class ScrollAnchor(val key: String, val offset: Int)

private val whitespaceRun = Regex("\\s+")

/**
 * 引用文本：压成一行并截断，空正文用占位文案；
 * 输入框已有内容时另起一行追加，不覆盖用户正在写的东西。
 */
internal fun quoteDraft(original: String, existing: String, placeholder: String, limit: Int = 80): String {
    val flattened = original.trim().replace(whitespaceRun, " ")
    val base = flattened.ifBlank { placeholder }
    val snippet = if (base.length > limit) base.take(limit - 1).trimEnd() + "…" else base
    val line = "> $snippet"
    return if (existing.isBlank()) line else existing.trimEnd() + "\n" + line
}

/** 选择器返回的文件元信息；系统拿不到的字段用默认值兜底。 */
private data class PickedAttachment(
    val name: String,
    val mimeType: String,
    val sizeBytes: Long,
)

private fun queryAttachment(context: Context, uri: Uri, fallbackName: String): PickedAttachment {
    var name: String? = null
    var size = 0L
    runCatching {
        context.contentResolver
            .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)
            ?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val nameIndex = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (nameIndex >= 0 && !cursor.isNull(nameIndex)) name = cursor.getString(nameIndex)
                    val sizeIndex = cursor.getColumnIndex(OpenableColumns.SIZE)
                    if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) size = cursor.getLong(sizeIndex)
                }
            }
    }
    val resolvedName = name?.takeIf { it.isNotBlank() } ?: fallbackName
    val mimeType = context.contentResolver.getType(uri)
        ?: java.net.URLConnection.guessContentTypeFromName(resolvedName)
        ?: "application/octet-stream"
    return PickedAttachment(resolvedName, mimeType, size)
}
