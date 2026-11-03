package dev.imbo.android.ui.screen

import android.app.Activity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChevronLeft
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import coil.compose.AsyncImage
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.imbo.android.BuildConfig
import dev.imbo.android.R
import dev.imbo.android.data.Account
import dev.imbo.android.data.BlockedAccount
import dev.imbo.android.data.Contact
import dev.imbo.android.data.Device
import dev.imbo.android.data.ForceError
import dev.imbo.android.data.ImboApi
import dev.imbo.android.data.LocaleStore
import dev.imbo.android.data.MockConfig
import dev.imbo.android.data.ServerConfigStore
import dev.imbo.android.data.UiState
import dev.imbo.android.data.mock.MockImboApi
import dev.imbo.android.ui.components.Avatar
import dev.imbo.android.ui.components.EmptyState
import dev.imbo.android.ui.components.GhostButton
import dev.imbo.android.ui.components.Hairline
import dev.imbo.android.ui.components.IconButton32
import dev.imbo.android.ui.components.ImboDialog
import dev.imbo.android.ui.components.localizedServerError
import dev.imbo.android.ui.components.ImboTextFieldDialog
import dev.imbo.android.ui.components.InsetField
import dev.imbo.android.ui.components.PrimaryButton
import dev.imbo.android.ui.components.ScreenRoot
import dev.imbo.android.ui.components.SectionLabel
import dev.imbo.android.ui.components.SelectionDot
import dev.imbo.android.ui.theme.ImboColor
import dev.imbo.android.ui.theme.ImboDimen
import dev.imbo.android.ui.theme.ImboMono
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

// ---------------------------------------------------------------- account

/** 头像上传 / 移除是独立状态：资料保存的 busy 不该把头像按钮一起锁住。 */
sealed interface AvatarState {
    data object Idle : AvatarState
    data object Uploading : AvatarState
    data object Removing : AvatarState
    data object Failed : AvatarState
}

@HiltViewModel
class AccountViewModel @Inject constructor(
    private val api: ImboApi,
    private val mock: MockImboApi,
    val mockConfig: MutableStateFlow<MockConfig>,
) : ViewModel() {

    private val _account = MutableStateFlow<UiState<Account>>(UiState.Loading)
    val account: StateFlow<UiState<Account>> = _account.asStateFlow()

    /** 自检：打一次 /v1/health，把版本号或错误原因显示在账号页。 */
    private val _health = MutableStateFlow<UiState<String>?>(null)
    val health: StateFlow<UiState<String>?> = _health.asStateFlow()

    fun checkHealth() {
        _health.value = UiState.Loading
        viewModelScope.launch {
            _health.value = try {
                val body = api.health()
                val version = Regex("\"version\"\\s*:\\s*\"([^\"]+)\"").find(body)?.groupValues?.get(1)
                UiState.Content(version ?: "ok")
            } catch (e: Exception) {
                UiState.Error(e.message ?: "error")
            }
        }
    }
    private val _avatar = MutableStateFlow<AvatarState>(AvatarState.Idle)
    val avatar: StateFlow<AvatarState> = _avatar.asStateFlow()

    private val _devices = MutableStateFlow<List<Device>>(emptyList())
    val devices: StateFlow<List<Device>> = _devices.asStateFlow()

    private val _blocked = MutableStateFlow<List<BlockedAccount>>(emptyList())
    val blocked: StateFlow<List<BlockedAccount>> = _blocked.asStateFlow()

    private val _busy = MutableStateFlow(false)
    val busy: StateFlow<Boolean> = _busy.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    private val _signedOut = MutableStateFlow(false)
    val signedOut: StateFlow<Boolean> = _signedOut.asStateFlow()

    /** 验证码发往哪个新邮箱：为空表示还没发出，非空才允许进入验证码那一步。 */
    private val _emailCodeSentTo = MutableStateFlow<String?>(null)
    val emailCodeSentTo: StateFlow<String?> = _emailCodeSentTo.asStateFlow()

    init { refresh() }

    fun refresh() {
        viewModelScope.launch {
            _account.value = try {
                UiState.Content(api.me())
            } catch (e: Exception) {
                UiState.Error(e.message ?: "error")
            }
            _devices.value = runCatching { api.devices() }.getOrDefault(emptyList())
            _blocked.value = runCatching { api.blocked() }.getOrDefault(emptyList())
        }
    }

    fun revokeDevice(id: String) {
        viewModelScope.launch {
            runCatching { api.revokeDevice(id) }
            _devices.value = runCatching { api.devices() }.getOrDefault(emptyList())
        }
    }

    fun rename(newName: String) = save { api.updateProfile(name = newName) }

    // 发码请求还在路上时用户可能已经关掉对话框：用自增编号丢掉过期结果，别让旧的待验证邮箱复活。
    private var emailCodeRequestId = 0

    /** 换邮箱第一步：验证码只发给新邮箱，发不出去就不许往下走。 */
    fun requestEmailCode(email: String) {
        if (_busy.value) return
        val requestId = ++emailCodeRequestId
        _busy.value = true
        _error.value = null
        viewModelScope.launch {
            try {
                api.requestEmailCode(email)
                if (requestId != emailCodeRequestId) return@launch
                _emailCodeSentTo.value = email
            } catch (e: Exception) {
                if (requestId == emailCodeRequestId) {
                    _emailCodeSentTo.value = null
                    _error.value = e.message ?: "error"
                }
            } finally {
                _busy.value = false
            }
        }
    }

    /** 换邮箱第二步：验证码错了保留待验证邮箱，用户直接重输就行，不必再发一次码。 */
    fun changeEmail(email: String, code: String) {
        if (_busy.value) return
        _busy.value = true
        _error.value = null
        viewModelScope.launch {
            try {
                _account.value = UiState.Content(api.changeEmail(email, code))
                _emailCodeSentTo.value = null
            } catch (e: Exception) {
                _error.value = e.message ?: "error"
            } finally {
                _busy.value = false
            }
        }
    }

    /** 关掉对话框就是放弃这次换邮箱，待验证邮箱不能留给下一次。 */
    fun cancelEmailChange() {
        emailCodeRequestId++
        _emailCodeSentTo.value = null
    }

    fun uploadAvatar(localUri: String) = updateAvatar(AvatarState.Uploading) { api.uploadAvatar(localUri) }

    fun clearAvatar() = updateAvatar(AvatarState.Removing) { api.clearAvatar() }

    /** 头像写操作独立走 pending / 失败状态，失败时保留原头像，不静默吞掉。 */
    private fun updateAvatar(pending: AvatarState, block: suspend () -> Account) {
        if (_avatar.value == AvatarState.Uploading || _avatar.value == AvatarState.Removing) return
        _avatar.value = pending
        viewModelScope.launch {
            _avatar.value = try {
                _account.value = UiState.Content(block())
                AvatarState.Idle
            } catch (_: Exception) {
                AvatarState.Failed
            }
        }
    }

    /** 写操作统一走 pending → 成功刷新 / 失败留错误，不静默吞掉（DATA-004）。 */
    private fun save(block: suspend () -> Account) {
        if (_busy.value) return
        _busy.value = true
        _error.value = null
        viewModelScope.launch {
            try {
                _account.value = UiState.Content(block())
            } catch (e: Exception) {
                _error.value = e.message ?: "error"
            } finally {
                _busy.value = false
            }
        }
    }

    fun signOut() {
        viewModelScope.launch {
            runCatching { api.signOut() }
            _signedOut.value = true
        }
    }

    /** 服务端删完账号后本地立刻回登录页；失败时保留账号并报错，别把用户甩在一个空页面上。 */
    fun deleteAccount() {
        if (_busy.value) return
        _busy.value = true
        _error.value = null
        viewModelScope.launch {
            try {
                api.deleteAccount()
                _signedOut.value = true
            } catch (e: Exception) {
                _error.value = e.message ?: "error"
            } finally {
                _busy.value = false
            }
        }
    }

    fun setLatency(ms: Long) {
        mockConfig.value = mockConfig.value.copy(latencyMs = ms)
        mock.poke()
        refresh()
    }

    fun setError(error: ForceError?) {
        mockConfig.value = mockConfig.value.copy(forceError = error)
        mock.poke()
        refresh()
    }

    fun setEmpty(empty: Boolean) {
        mockConfig.value = mockConfig.value.copy(emptyData = empty)
        mock.poke()
        refresh()
    }
}

@Composable
fun AccountScreen(
    onBack: () -> Unit,
    onBlocked: () -> Unit,
    onSignedOut: () -> Unit,
    viewModel: AccountViewModel = hiltViewModel(),
) {
    val accountState by viewModel.account.collectAsStateWithLifecycle()
   val avatarState by viewModel.avatar.collectAsStateWithLifecycle()
    val devices by viewModel.devices.collectAsStateWithLifecycle()
    val blocked by viewModel.blocked.collectAsStateWithLifecycle()
    val signedOut by viewModel.signedOut.collectAsStateWithLifecycle()
    val busy by viewModel.busy.collectAsStateWithLifecycle()
    val error by viewModel.error.collectAsStateWithLifecycle()
    val emailCodeSentTo by viewModel.emailCodeSentTo.collectAsStateWithLifecycle()

    // 系统 Photo Picker：不需要存储权限，返回 content:// 地址
    val pickPicture = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri != null) viewModel.uploadAvatar(uri.toString())
    }

    var showRename by remember { mutableStateOf(false) }
    var showEmail by remember { mutableStateOf(false) }
    var showSubscribe by remember { mutableStateOf(false) }
    var showDelete by remember { mutableStateOf(false) }

    if (signedOut) onSignedOut()

    ScreenRoot {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(ImboDimen.BrandBar)
                .padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton32(onClick = onBack) {
                Icon(Icons.Outlined.ChevronLeft, contentDescription = stringResource(R.string.back), tint = ImboColor.Tertiary)
            }
            Text(
                text = stringResource(R.string.account_title),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            // 套餐状态跟着账号走：试用显示剩余天数，已订阅显示「已订阅」，free 不显示
            val access = (accountState as? UiState.Content)?.data?.access
            val statusText = when (access) {
                is dev.imbo.android.data.Access.Trial -> stringResource(R.string.trial_days_left, access.daysLeft)
                is dev.imbo.android.data.Access.Paid -> stringResource(R.string.subscribed)
                else -> null
            }
            if (statusText != null) {
                Text(
                    text = statusText,
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                )
            }
            Spacer(Modifier.size(8.dp))
            GhostButton(text = stringResource(R.string.subscribe), onClick = { showSubscribe = true })
        }
        Hairline(vertical = false)

        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = ImboDimen.Gutter, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            when (val s = accountState) {
                is UiState.Loading -> LoadingBlock()
                is UiState.Error -> ErrorBlock(s.message) { viewModel.refresh() }
                is UiState.Content -> AccountContent(
                    account = s.data,
                    devices = devices,
                    blocked = blocked,
                    busy = busy,
                    error = error,
                    emailCodeSentTo = emailCodeSentTo,
                    avatarState = avatarState,
                    onRename = { showRename = true },
                    onChangeEmail = { showEmail = true },
                    onChangePicture = {
                        pickPicture.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                    },
                    onRemovePicture = { viewModel.clearAvatar() },
                    onBlocked = onBlocked,
                    onRevoke = { viewModel.revokeDevice(it) },
                    onDeleteRequested = { showDelete = true },
                    onSignOut = { viewModel.signOut() },
                    onSubscribe = { showSubscribe = true },
                    viewModel = viewModel,
                )
            }
        }
    }

    val account = (accountState as? UiState.Content)?.data
    val codeEmail = emailCodeSentTo
    if (showRename && account != null) {
        ImboTextFieldDialog(
            title = stringResource(R.string.rename_title),
            label = stringResource(R.string.name_label),
            initial = account.name,
            confirmLabel = stringResource(R.string.save),
            onConfirm = { viewModel.rename(it); showRename = false },
            onDismiss = { showRename = false },
        )
    }
    // 第一步只发码，关了就不往下走：成功与否看 emailCodeSentTo，发送失败时对话框留在原处
    if (showEmail && account != null && codeEmail == null) {
        ImboTextFieldDialog(
            title = stringResource(R.string.change_email_title),
            label = stringResource(R.string.email_label),
            initial = "",
            hint = stringResource(R.string.email_change_body),
            confirmLabel = stringResource(R.string.email_change_send),
            onConfirm = { viewModel.requestEmailCode(it) },
            onDismiss = { showEmail = false; viewModel.cancelEmailChange() },
        )
    }
    // 第二步靠待验证邮箱自己撑住：验证码错了它还非空，对话框不会消失；换成功后清空即关闭
    if (codeEmail != null) {
        ImboTextFieldDialog(
            title = stringResource(R.string.email_change_code_title),
            label = stringResource(R.string.code_label),
            initial = "",
            hint = stringResource(R.string.email_change_code_body, codeEmail),
            confirmLabel = stringResource(R.string.email_change_confirm),
            onConfirm = {
                // 先收掉第一步的开关，否则成功后待验证邮箱一清空，第一步会闪回来
                showEmail = false
                viewModel.changeEmail(codeEmail, it)
            },
            onDismiss = { showEmail = false; viewModel.cancelEmailChange() },
        )
    }
    if (showSubscribe) {
        ImboDialog(
            title = stringResource(R.string.subscribe_title),
            body = stringResource(R.string.subscribe_unavailable),
            confirmLabel = stringResource(R.string.cancel),
            onConfirm = { showSubscribe = false },
            onDismiss = { showSubscribe = false },
            dismissLabel = stringResource(R.string.cancel),
        )
    }
    if (showDelete) {
        ImboDialog(
            title = stringResource(R.string.delete_account_title),
            body = stringResource(R.string.delete_account_body),
            confirmLabel = stringResource(R.string.delete_account),
            onConfirm = { showDelete = false; viewModel.deleteAccount() },
            onDismiss = { showDelete = false },
            destructive = true,
        )
    }
}

@Composable
private fun AccountContent(
    account: Account,
    devices: List<Device>,
    blocked: List<BlockedAccount>,
    busy: Boolean,
    error: String?,
    emailCodeSentTo: String?,
    avatarState: AvatarState,
    onRename: () -> Unit,
    onChangeEmail: () -> Unit,
    onChangePicture: () -> Unit,
    onRemovePicture: () -> Unit,
    onBlocked: () -> Unit,
    onRevoke: (String) -> Unit,
    onDeleteRequested: () -> Unit,
    onSignOut: () -> Unit,
    onSubscribe: () -> Unit,
    viewModel: AccountViewModel,
) {
    val context = LocalContext.current
    // 自检结果：连不上服务端时这里会给出原因
    val health by viewModel.health.collectAsStateWithLifecycle()
    val currentLanguage = remember { LocaleStore.read(context) }

    val uploading = avatarState == AvatarState.Uploading
    val removing = avatarState == AvatarState.Removing
    val avatarBusy = uploading || removing
    val changePictureLabel = stringResource(R.string.change_picture)

    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(
                modifier = Modifier
                    .size(ImboDimen.AvatarLarge)
                    .clip(CircleShape)
                    .background(ImboColor.SecondaryContainer)
                    .semantics { contentDescription = changePictureLabel }
                    .clickable(
                        enabled = !avatarBusy,
                        onClickLabel = changePictureLabel,
                        role = Role.Button,
                        onClick = onChangePicture,
                    ),
                contentAlignment = Alignment.Center,
            ) {
                if (account.avatarUrl != null) {
                    AsyncImage(
                        model = account.avatarUrl,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier.fillMaxSize().clip(CircleShape),
                    )
                } else {
                    Text(account.name.take(1).uppercase(), color = ImboColor.OnSecondaryContainer, fontFamily = ImboMono, fontSize = 22.sp)
                }
            }
            Spacer(Modifier.size(12.dp))
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                GhostButton(
                    text = if (uploading) stringResource(R.string.avatar_uploading) else stringResource(R.string.change_picture),
                    onClick = onChangePicture,
                    height = 28,
                    enabled = !avatarBusy,
                )
                if (account.avatarUrl != null) {
                    GhostButton(
                        text = if (removing) stringResource(R.string.avatar_removing) else stringResource(R.string.remove_picture),
                        onClick = onRemovePicture,
                        height = 28,
                        enabled = !avatarBusy,
                    )
                }
                Text(
                    text = stringResource(R.string.picture_formats),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                )
                if (avatarState == AvatarState.Failed) {
                    Text(
                        text = stringResource(R.string.avatar_upload_failed),
                        color = ImboColor.Error,
                        fontFamily = ImboMono,
                        fontSize = 12.sp,
                    )
                }
            }
        }
    }

    if (error != null) {
        Text(
            text = error,
            color = ImboColor.Error,
            fontFamily = ImboMono,
            fontSize = 12.sp,
        )
    }

    // 验证码发出后告诉用户去了哪个邮箱，借 error 同一处位置渲染，不为此另加提示框架
    if (emailCodeSentTo != null) {
        Text(
            text = stringResource(R.string.email_change_sent, emailCodeSentTo),
            color = ImboColor.Success,
            fontFamily = ImboMono,
            fontSize = 12.sp,
        )
    }

    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            text = account.name,
            color = ImboColor.OnBackground,
            fontFamily = ImboMono,
            fontSize = 14.sp,
            modifier = Modifier.weight(1f),
        )
        GhostButton(text = stringResource(R.string.edit), onClick = onRename, enabled = !busy)
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            text = account.email,
            color = ImboColor.OnBackground,
            fontFamily = ImboMono,
            fontSize = 14.sp,
            modifier = Modifier.weight(1f),
        )
        GhostButton(text = stringResource(R.string.change), onClick = onChangeEmail, enabled = !busy)
    }

    Column {
        Text(
            text = stringResource(R.string.account_id),
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 13.sp,
            fontWeight = FontWeight.Medium,
        )
        Spacer(Modifier.height(6.dp))
        Box(
            modifier = Modifier
                .clip(RoundedCornerShape(4.dp))
                .background(ImboColor.SecondaryContainer)
                .padding(horizontal = 10.dp, vertical = 4.dp),
        ) {
            Text(account.accountId, color = ImboColor.OnBackground, fontFamily = ImboMono, fontSize = 13.sp)
        }
        Spacer(Modifier.height(6.dp))
        Text(
            text = stringResource(R.string.account_id_hint),
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
        )
    }

    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(
            // 服务端的 free / expired 不是「试用还剩 0 天」，别把两种状态说成一句
            text = when (val access = account.access) {
                is dev.imbo.android.data.Access.Trial -> stringResource(R.string.free_trial_days, access.daysLeft)
                is dev.imbo.android.data.Access.Paid -> stringResource(R.string.subscribed)
                dev.imbo.android.data.Access.Expired -> stringResource(R.string.access_expired)
                dev.imbo.android.data.Access.Free -> stringResource(R.string.access_free)
            },
            color = ImboColor.OnBackground,
            fontFamily = ImboMono,
            fontSize = 14.sp,
        )
        Text(
            text = stringResource(R.string.subscribe_hint),
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            GhostButton(text = stringResource(R.string.plan_yearly), onClick = onSubscribe, height = 32)
            GhostButton(text = stringResource(R.string.plan_monthly), onClick = onSubscribe, height = 32)
        }
        Text(
            text = stringResource(R.string.taxes_note),
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
        )
    }

    Column {
        SectionLabel(stringResource(R.string.language))
        LanguageRow(stringResource(R.string.language_english), currentLanguage == "en") {
            LocaleStore.write(context, "en")
            (context as? Activity)?.recreate()
        }
        LanguageRow(stringResource(R.string.language_chinese), currentLanguage == "zh") {
            LocaleStore.write(context, "zh")
            (context as? Activity)?.recreate()
        }
        LanguageRow(stringResource(R.string.language_system), currentLanguage == null) {
            LocaleStore.write(context, null)
            (context as? Activity)?.recreate()
        }
        Text(
            text = stringResource(R.string.language_system_hint),
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
            modifier = Modifier.padding(horizontal = ImboDimen.Gutter),
        )
    }

    Column {
        SectionLabel(stringResource(R.string.devices))
        devices.forEach { device ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = ImboDimen.Gutter, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(device.name, color = ImboColor.OnBackground, fontFamily = ImboMono, fontSize = 14.sp)
                    Text(
                        text = "${device.platform}  ·  ${seenLabel(device.lastSeenMinutes)}",
                        color = ImboColor.Tertiary,
                        fontFamily = ImboMono,
                        fontSize = 12.sp,
                    )
                }
                GhostButton(text = stringResource(R.string.sign_out), onClick = { onRevoke(device.id) })
            }
        }
        Text(
            text = stringResource(R.string.devices_hint),
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
            modifier = Modifier.padding(horizontal = ImboDimen.Gutter),
        )
    }

    // 换服务器等于换一套账号：改完立刻退出登录，别让旧 token 跟着新地址跑
    var showServer by remember { mutableStateOf(false) }
    var serverError by remember { mutableStateOf<String?>(null) }
    var serverBase by remember { mutableStateOf(ServerConfigStore.read(context)) }
    // 推送：本机 ntfy 话题 + 订阅链接；换手机或重装会换话题
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    val ntfyTopic = dev.imbo.android.data.NtfyStore.topic(context)
        // ntfy 地址运行时可变：不同团队部署在自己的域名上，不该固化进 APK
    var showNtfy by remember { mutableStateOf(false) }
val ntfyUrl = dev.imbo.android.data.NtfyStore.subscribeUrl(context)
    Column {
        SectionLabel(stringResource(R.string.push_label))
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = ImboDimen.Gutter),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = if (ntfyUrl != null) {
                    stringResource(R.string.push_topic_value, ntfyTopic)
                } else {
                    stringResource(R.string.push_topic_only, ntfyTopic)
                },
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = stringResource(R.string.push_set_server),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.clickable { showNtfy = true },
            )
            Spacer(Modifier.width(10.dp))
            Text(
                text = stringResource(R.string.chat_copy),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.clickable {
                    clipboard.setText(androidx.compose.ui.text.AnnotatedString(ntfyUrl ?: ntfyTopic))
                },
            )
        }
        Spacer(Modifier.height(8.dp))
    }
    if (showNtfy) {
        ImboTextFieldDialog(
            title = stringResource(R.string.push_set_server),
            label = stringResource(R.string.push_server_label),
            initial = dev.imbo.android.data.NtfyStore.baseUrl(context) ?: BuildConfig.NTFY_URL,
            confirmLabel = stringResource(R.string.save),
            hint = stringResource(R.string.push_server_hint),
            onConfirm = { value -> dev.imbo.android.data.NtfyStore.writeBaseUrl(context, value); showNtfy = false },
            onDismiss = { showNtfy = false },
        )
    }
    // 自检：点一下确认手机到服务端是通的，省得靠登录失败去猜
    Column {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = ImboDimen.Gutter),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = when (val h = health) {
                    null -> stringResource(R.string.check_connection)
                    is UiState.Loading -> stringResource(R.string.checking)
                    is UiState.Content -> stringResource(R.string.check_ok, h.data)
                    is UiState.Error -> stringResource(R.string.check_failed, h.message)
                },
                color = if (health is UiState.Error) ImboColor.Error else ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = stringResource(R.string.check_connection),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.clickable(enabled = health != UiState.Loading) { viewModel.checkHealth() },
            )
        }
        Spacer(Modifier.height(8.dp))
    }
    Column {
        SectionLabel(stringResource(R.string.server_label))
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = ImboDimen.Gutter),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = serverBase?.let { ServerConfigStore.displayHost(it) }
                    ?: stringResource(R.string.server_using_default, ServerConfigStore.displayHost(BuildConfig.BASE_URL)),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = stringResource(R.string.change),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.clickable(enabled = !busy) { showServer = true },
            )
        }
    }
    if (showServer) {
        ImboTextFieldDialog(
            title = stringResource(R.string.server_address_title),
            label = stringResource(R.string.server_label),
            initial = serverBase.orEmpty(),
            confirmLabel = stringResource(R.string.save),
            hint = serverError ?: stringResource(R.string.server_address_hint),
            onConfirm = { value ->
                val normalized = ServerConfigStore.normalize(value)
                serverError = when {
                    normalized == null -> context.getString(R.string.server_address_invalid)
                    !BuildConfig.DEBUG && normalized.startsWith("http://") ->
                        context.getString(R.string.server_address_https_required)
                    else -> null
                }
                if (serverError == null) {
                    ServerConfigStore.write(context, normalized)
                    serverBase = normalized
                    showServer = false
                    // token 属于旧服务器，换个地址就得重新登录
                    onSignOut()
                }
            },
            onDismiss = { showServer = false; serverError = null },
        )
    }

    Column {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = ImboDimen.Gutter),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = stringResource(R.string.blocked),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 13.sp,
                fontWeight = FontWeight.Medium,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = stringResource(R.string.manage),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.clickable { onBlocked() },
            )
        }
        Spacer(Modifier.height(6.dp))
        Text(
            text = if (blocked.isEmpty()) stringResource(R.string.blocked_hint) else blocked.joinToString(", ") { it.name },
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
            modifier = Modifier.padding(horizontal = ImboDimen.Gutter),
        )
    }

    // 换账号 / 交回电脑都靠这一行：以前只能靠「删除账号」顺带退出，等于没有退出登录
    Text(
        text = stringResource(R.string.sign_out_account),
        color = if (busy) ImboColor.Tertiary else ImboColor.OnBackground,
        fontFamily = ImboMono,
        fontSize = 12.sp,
        modifier = Modifier
            .padding(horizontal = ImboDimen.Gutter)
            .clickable(enabled = !busy) { onSignOut() },
    )

    Text(
        text = stringResource(R.string.delete_account),
        color = if (busy) ImboColor.Tertiary else ImboColor.Error,
        fontFamily = ImboMono,
        fontSize = 12.sp,
        modifier = Modifier
            .padding(horizontal = ImboDimen.Gutter)
            .clickable(enabled = !busy) { onDeleteRequested() },
    )

    if (BuildConfig.DEBUG) {
        MockScenarioPanel(viewModel)
    }
}

/** 4320 分钟这种值不该按分钟显示，按小时 / 天分档。 */
@Composable
private fun seenLabel(minutes: Int): String = when {
    minutes < 1 -> stringResource(R.string.device_seen_now)
    minutes < 60 -> stringResource(R.string.device_seen_minutes, minutes)
    minutes < 60 * 24 -> stringResource(R.string.device_seen_hours, minutes / 60)
    else -> stringResource(R.string.device_seen_days, minutes / (60 * 24))
}

@Composable
private fun LanguageRow(label: String, selected: Boolean, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable { onClick() }
            .padding(horizontal = ImboDimen.Gutter, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(modifier = Modifier.size(20.dp), contentAlignment = Alignment.Center) {
            SelectionDot(selected = selected)
        }
        Spacer(Modifier.size(12.dp))
        Text(
            text = label,
            color = if (selected) ImboColor.OnBackground else ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 14.sp,
        )
    }
}

/** 验收用的场景开关：造出加载、空、错误、慢网，不依赖服务端。 */
@Composable
private fun MockScenarioPanel(viewModel: AccountViewModel) {
    val config by viewModel.mockConfig.collectAsStateWithLifecycle()
    Column(modifier = Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        SectionLabel("Mock scenario")
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            GhostButton("150ms", { viewModel.setLatency(150) }, height = 28)
            GhostButton("0ms", { viewModel.setLatency(0) }, height = 28)
            GhostButton("1500ms", { viewModel.setLatency(1500) }, height = 28)
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            GhostButton("401", { viewModel.setError(ForceError.Unauthorized) }, height = 28)
            GhostButton("500", { viewModel.setError(ForceError.ServerError) }, height = 28)
            GhostButton("ok", { viewModel.setError(null) }, height = 28)
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            GhostButton(if (config.emptyData) "empty: on" else "empty: off", { viewModel.setEmpty(!config.emptyData) }, height = 28)
        }
    }
}

// ---------------------------------------------------------------- blocked

@HiltViewModel
class BlockedViewModel @Inject constructor(private val api: ImboApi) : ViewModel() {
    private val _state = MutableStateFlow<UiState<List<BlockedAccount>>>(UiState.Loading)
    val state: StateFlow<UiState<List<BlockedAccount>>> = _state.asStateFlow()

    private val _busyId = MutableStateFlow<String?>(null)
    val busyId: StateFlow<String?> = _busyId.asStateFlow()

    init { refresh() }

    fun refresh() {
        viewModelScope.launch {
            _state.value = try {
                UiState.Content(api.blocked())
            } catch (e: Exception) {
                UiState.Error(e.message ?: "error")
            }
        }
    }

    fun unblock(id: String) {
        viewModelScope.launch {
            _busyId.value = id
            try {
                api.unblock(id)
                _state.value = UiState.Content(api.blocked())
            } catch (e: Exception) {
                _state.value = UiState.Error(e.message ?: "error")
            } finally {
                _busyId.value = null
            }
        }
    }
}

@Composable
fun BlockedScreen(onBack: () -> Unit, viewModel: BlockedViewModel = hiltViewModel()) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val busyId by viewModel.busyId.collectAsStateWithLifecycle()

    ScreenRoot {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(ImboDimen.BrandBar)
                .padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton32(onClick = onBack) {
                Icon(Icons.Outlined.ChevronLeft, contentDescription = stringResource(R.string.back), tint = ImboColor.Tertiary)
            }
            Text(
                text = stringResource(R.string.blocked),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
            )
        }
        Hairline(vertical = false)
        when (val s = state) {
            is UiState.Loading -> LoadingBlock()
            is UiState.Error -> ErrorBlock(s.message) { viewModel.refresh() }
            is UiState.Content -> Column(modifier = Modifier.fillMaxSize().padding(horizontal = 8.dp, vertical = 12.dp)) {
                Text(
                    text = stringResource(R.string.blocked_hint),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 8.dp),
                )
                if (s.data.isEmpty()) {
                    EmptyState(stringResource(R.string.blocked), stringResource(R.string.blocked_hint))
                } else {
                    s.data.forEach { blocked ->
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Avatar(letter = blocked.name.take(1).uppercase())
                            Text(
                                text = blocked.name,
                                color = ImboColor.OnBackground,
                                fontFamily = ImboMono,
                                fontSize = 14.sp,
                                modifier = Modifier.weight(1f).padding(start = 12.dp),
                            )
                            GhostButton(
                                text = stringResource(R.string.unblock),
                                onClick = { viewModel.unblock(blocked.id) },
                                enabled = busyId != blocked.id,
                            )
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- add contact

sealed interface AddContactState {
    data object Idle : AddContactState
    data object Working : AddContactState
    data object Sent : AddContactState
    data class Failed(val message: String) : AddContactState
}

@HiltViewModel
class AddContactViewModel @Inject constructor(private val api: ImboApi) : ViewModel() {
    private val _state = MutableStateFlow<AddContactState>(AddContactState.Idle)
    val state: StateFlow<AddContactState> = _state.asStateFlow()

    fun submit(accountId: String, note: String?) {
        if (_state.value == AddContactState.Working) return
        _state.value = AddContactState.Working
        viewModelScope.launch {
            _state.value = try {
                api.addContact(accountId, note)
                AddContactState.Sent
            } catch (e: Exception) {
                AddContactState.Failed(e.message ?: "error")
            }
        }
    }

    /** 邮箱和账号 ID 走同一条「发请求→对方接受」的流程，服务端按正文里的字段区分。 */
    fun submitByEmail(email: String, note: String?) {
        if (_state.value == AddContactState.Working) return
        _state.value = AddContactState.Working
        viewModelScope.launch {
            _state.value = try {
                api.addContactByEmail(email, note)
                AddContactState.Sent
            } catch (e: Exception) {
                AddContactState.Failed(e.message ?: "error")
            }
        }
    }
}

@Composable
fun AddContactScreen(
    onDone: () -> Unit,
    onBack: () -> Unit,
    viewModel: AddContactViewModel = hiltViewModel(),
) {
    var value by remember { mutableStateOf("") }
    var note by remember { mutableStateOf("") }
    val state by viewModel.state.collectAsStateWithLifecycle()

    ScreenRoot {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(ImboDimen.BrandBar)
                .padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton32(onClick = onBack) {
                Icon(Icons.Outlined.ChevronLeft, contentDescription = stringResource(R.string.back), tint = ImboColor.Tertiary)
            }
            Text(
                text = stringResource(R.string.add_contact),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
            )
        }
        Hairline(vertical = false)
        Column(
            modifier = Modifier.fillMaxSize().padding(horizontal = ImboDimen.Gutter, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            InsetField(
                label = stringResource(R.string.account_id_or_email),
                value = value,
                placeholder = "81234567",
                onValueChange = { value = it.trim() },
                hint = stringResource(R.string.add_contact_hint),
                keyboardType = if (value.contains('@')) KeyboardType.Email else KeyboardType.Number,
            )
            InsetField(
                label = stringResource(R.string.add_contact_note),
                value = note,
                placeholder = "bob, api team",
                onValueChange = { note = it.take(120) },
                hint = stringResource(R.string.add_contact_note_hint),
            )

            when (val s = state) {
                is AddContactState.Sent -> Text(
                    text = stringResource(R.string.add_contact_sent),
                    color = ImboColor.Success,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                )
                is AddContactState.Failed -> Text(
                    text = localizedServerError(s.message) ?: s.message,
                    color = ImboColor.Error,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                )
                else -> Unit
            }
            PrimaryButton(
                text = if (state == AddContactState.Working) stringResource(R.string.sending) else stringResource(R.string.add_contact),
                onClick = { if (value.contains('@')) viewModel.submitByEmail(value, note) else viewModel.submit(value, note) },
                enabled = (value.length == 8 && value.all { it.isDigit() } || value.contains('@')) &&
                    state != AddContactState.Working,
                modifier = Modifier.fillMaxWidth(),
            )
            if (state == AddContactState.Sent) {
                GhostButton(text = stringResource(R.string.back), onClick = onDone)
            }
        }
    }
}

// ---------------------------------------------------------------- new group

sealed interface NewGroupState {
    data object Idle : NewGroupState
    data object Working : NewGroupState
    data class Created(val conversationId: Long) : NewGroupState
    data class Failed(val message: String) : NewGroupState
}

@HiltViewModel
class NewGroupViewModel @Inject constructor(private val api: ImboApi) : ViewModel() {
    private val _contacts = MutableStateFlow<UiState<List<Contact>>>(UiState.Loading)
    val contacts: StateFlow<UiState<List<Contact>>> = _contacts.asStateFlow()

    private val _selected = MutableStateFlow<Set<String>>(emptySet())
    val selected: StateFlow<Set<String>> = _selected.asStateFlow()

    private val _state = MutableStateFlow<NewGroupState>(NewGroupState.Idle)
    val state: StateFlow<NewGroupState> = _state.asStateFlow()

    init { refresh() }

    fun refresh() {
        viewModelScope.launch {
            _contacts.value = try {
                UiState.Content(api.contacts().filterNot { it.agent })
            } catch (e: Exception) {
                UiState.Error(e.message ?: "error")
            }
            _agents.value = runCatching { api.agents() }.getOrDefault(emptyList())
        }
    }

    fun toggle(id: String) {
        _selected.value = if (_selected.value.contains(id)) _selected.value - id else _selected.value + id
    }

    private val _agents = MutableStateFlow<List<dev.imbo.android.data.AgentSession>>(emptyList())
    val agents: StateFlow<List<dev.imbo.android.data.AgentSession>> = _agents.asStateFlow()

    private val _selectedSessions = MutableStateFlow<Set<String>>(emptySet())
    val selectedSessions: StateFlow<Set<String>> = _selectedSessions.asStateFlow()

    fun toggleSession(id: String) {
        val current = _selectedSessions.value
        _selectedSessions.value = if (current.contains(id)) current - id else current + id
    }

    fun create(name: String) {
        if (_state.value == NewGroupState.Working) return
        _state.value = NewGroupState.Working
        viewModelScope.launch {
            _state.value = try {
                NewGroupState.Created(api.createGroup(name, _selected.value.toList(), _selectedSessions.value.toList()).id)
            } catch (e: Exception) {
                NewGroupState.Failed(e.message ?: "error")
            }
        }
    }
}

@Composable
fun NewGroupScreen(
    onCreated: (Long?) -> Unit,
    onBack: () -> Unit,
    viewModel: NewGroupViewModel = hiltViewModel(),
) {
    var name by remember { mutableStateOf("") }
    val contactsState by viewModel.contacts.collectAsStateWithLifecycle()
    val selected by viewModel.selected.collectAsStateWithLifecycle()
    val agents by viewModel.agents.collectAsStateWithLifecycle()
    val duplicatedSessionTitles = agents.groupBy { it.title }.filterValues { it.size > 1 }.keys
    val selectedSessions by viewModel.selectedSessions.collectAsStateWithLifecycle()
    val state by viewModel.state.collectAsStateWithLifecycle()

    ScreenRoot {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(ImboDimen.BrandBar)
                .padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton32(onClick = onBack) {
                Icon(Icons.Outlined.ChevronLeft, contentDescription = stringResource(R.string.back), tint = ImboColor.Tertiary)
            }
            Text(
                text = stringResource(R.string.new_group),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
            )
        }
        Hairline(vertical = false)
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = ImboDimen.Gutter, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            InsetField(
                label = stringResource(R.string.name_label),
                value = name,
                placeholder = "v2.3 release",
                onValueChange = { name = it },
            )
            SectionLabel(stringResource(R.string.new_group_members))
            when (val s = contactsState) {
                is UiState.Loading -> Text(
                    text = stringResource(R.string.loading),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                )
                is UiState.Error -> Text(
                    text = s.message,
                    color = ImboColor.Error,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                )
                is UiState.Content -> {
                    if (s.data.isEmpty()) {
                        Text(
                            text = stringResource(R.string.new_group_no_contacts),
                            color = ImboColor.Tertiary,
                            fontFamily = ImboMono,
                            fontSize = 12.sp,
                        )
                    } else {
                        s.data.forEach { contact ->
                            val active = selected.contains(contact.id)
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .clickable { viewModel.toggle(contact.id) }
                                    .padding(horizontal = ImboDimen.Gutter, vertical = 8.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Box(modifier = Modifier.size(20.dp), contentAlignment = Alignment.Center) {
                                    if (active) Text("✓", color = ImboColor.OnBackground, fontFamily = ImboMono, fontSize = 14.sp)
                                }
                                Spacer(Modifier.size(12.dp))
                                Avatar(letter = contact.name.take(1).uppercase())
                                Text(
                                    text = contact.name,
                                    color = if (active) ImboColor.OnBackground else ImboColor.Tertiary,
                                    fontFamily = ImboMono,
                                    fontSize = 14.sp,
                                    modifier = Modifier.padding(start = 12.dp),
                                )
                            }
                        }
                    }
                }
            }
            SectionLabel(stringResource(R.string.new_group_sessions))
            if (agents.isEmpty()) {
                Text(
                    text = stringResource(R.string.new_group_no_sessions),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                )
            } else {
                agents.forEach { session ->
                    val active = selectedSessions.contains(session.id)
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { viewModel.toggleSession(session.id) }
                            .padding(horizontal = ImboDimen.Gutter, vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Box(modifier = Modifier.size(20.dp), contentAlignment = Alignment.Center) {
                            if (active) Text("✓", color = ImboColor.OnBackground, fontFamily = ImboMono, fontSize = 14.sp)
                        }
                        Spacer(Modifier.size(12.dp))
                        Column {
                            Text(
                                text = if (session.title in duplicatedSessionTitles) "${session.title} · ${session.id.takeLast(6)}" else session.title,
                                color = if (active) ImboColor.OnBackground else ImboColor.Tertiary,
                                fontFamily = ImboMono,
                                fontSize = 14.sp,
                            )
                            Text(
                                text = listOfNotNull(session.tool, session.deviceName).joinToString(" · "),
                                color = ImboColor.Tertiary,
                                fontFamily = ImboMono,
                                fontSize = 11.sp,
                            )
                        }
                    }
                }
            }

            if (state is NewGroupState.Failed) {
                Text(
                    text = (state as NewGroupState.Failed).message,
                    color = ImboColor.Error,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                )
            }
            Text(
                text = stringResource(R.string.new_group_invite_hint),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
            )
            PrimaryButton(
                text = stringResource(R.string.new_group_create),
                onClick = { viewModel.create(name) },
                enabled = name.isNotBlank() && state != NewGroupState.Working,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }

    (state as? NewGroupState.Created)?.let { created ->
        androidx.compose.runtime.LaunchedEffect(created) { onCreated(created.conversationId) }
    }
}
