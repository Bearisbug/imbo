package dev.imbo.android.ui.screen

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.imbo.android.BuildConfig
import dev.imbo.android.R
import dev.imbo.android.data.Account
import dev.imbo.android.data.ServerConfigStore
import dev.imbo.android.data.ImboApi
import dev.imbo.android.ui.components.HSpacer
import dev.imbo.android.ui.components.ImboTextFieldDialog
import dev.imbo.android.ui.components.InsetField
import dev.imbo.android.ui.components.PrimaryButton
import dev.imbo.android.ui.components.ScreenRoot
import dev.imbo.android.ui.components.Wordmark
import dev.imbo.android.ui.theme.ImboColor
import dev.imbo.android.ui.theme.ImboDimen
import dev.imbo.android.ui.theme.ImboMono
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

sealed interface AuthUiState {
    data object Idle : AuthUiState
    data object Working : AuthUiState
    data class CodeSent(val email: String) : AuthUiState
    data class Failed(val message: String) : AuthUiState
    data class Done(val account: Account, val newAccount: Boolean) : AuthUiState
}

@HiltViewModel
class AuthViewModel @Inject constructor(private val api: ImboApi) : ViewModel() {

    private val _state = MutableStateFlow<AuthUiState>(AuthUiState.Idle)
    val state: StateFlow<AuthUiState> = _state.asStateFlow()

    private var pendingName: String? = null

    fun requestCode(email: String, name: String? = null) {
        if (_state.value == AuthUiState.Working) return
        pendingName = name
        _state.value = AuthUiState.Working
        viewModelScope.launch {
            _state.value = try {
                api.requestCode(email)
                AuthUiState.CodeSent(email)
            } catch (e: Exception) {
                AuthUiState.Failed(e.message ?: "error")
            }
        }
    }

    fun verify(email: String, code: String) {
        if (_state.value == AuthUiState.Working) return
        _state.value = AuthUiState.Working
        viewModelScope.launch {
            _state.value = try {
                val info = api.createSession(email, code, pendingName)
                AuthUiState.Done(info.account, info.newAccount)
            } catch (e: Exception) {
                AuthUiState.Failed(e.message ?: "error")
            }
        }
    }

    fun reset() {
        _state.value = AuthUiState.Idle
    }
}

/** mock 抛的是英文文案，这里映射到本地化字符串；未知错误原样显示，真实实现改为按状态码映射。 */
@Composable
private fun authErrorText(message: String): String = when {
    message.contains("valid email", ignoreCase = true) -> stringResource(R.string.auth_err_email)
    message.contains("code", ignoreCase = true) -> stringResource(R.string.auth_err_code)
    else -> message
}

/** 登录页只显示主机部分，完整地址留在对话框里，免得一行挤满整个宽度。 */
private fun hostOf(base: String): String = ServerConfigStore.displayHost(base)

@Composable
fun LoginScreen(
    onCodeSent: (String) -> Unit,
    onRegister: () -> Unit,
    viewModel: AuthViewModel = hiltViewModel(),
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    // 上次登录过的邮箱直接填好，少打一遍；没有就留空
    var email by remember { mutableStateOf(dev.imbo.android.data.LastLoginStore.read(context).orEmpty()) }
    var showServerDialog by remember { mutableStateOf(false) }
    var serverBase by remember { mutableStateOf(ServerConfigStore.read(context)) }
    var serverError by remember { mutableStateOf<String?>(null) }
    val state by viewModel.state.collectAsStateWithLifecycle()

    LaunchedEffect(state) {
        (state as? AuthUiState.CodeSent)?.let {
            dev.imbo.android.data.LastLoginStore.write(context, it.email)
            onCodeSent(it.email)
        }
    }

    ScreenRoot {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(horizontal = ImboDimen.Gutter),
            verticalArrangement = Arrangement.Center,
        ) {
            Wordmark()
            Spacer(Modifier.height(32.dp))
            Text(
                text = stringResource(R.string.sign_in_title),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 22.sp,
                lineHeight = 28.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                text = stringResource(R.string.sign_in_body),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                lineHeight = 25.sp,
            )
            Spacer(Modifier.height(24.dp))
            InsetField(
                label = stringResource(R.string.email_label),
                value = email,
                placeholder = stringResource(R.string.email_placeholder),
                onValueChange = { email = it.trim(); if (state is AuthUiState.Failed) viewModel.reset() },
                keyboardType = KeyboardType.Email,
            )
            Spacer(Modifier.height(24.dp))
            if (state is AuthUiState.Failed) {
                Text(
                    text = authErrorText((state as AuthUiState.Failed).message),
                    color = ImboColor.Error,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                )
                Spacer(Modifier.height(12.dp))
            }
            PrimaryButton(
                text = if (state == AuthUiState.Working) stringResource(R.string.sending) else stringResource(R.string.continue_action),
                onClick = { viewModel.requestCode(email) },
                enabled = email.contains("@") && state != AuthUiState.Working,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(40.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = stringResource(R.string.first_time),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 14.sp,
                )
                HSpacer(4)
                Text(
                    text = stringResource(R.string.create_account),
                    color = ImboColor.OnBackground,
                    fontFamily = ImboMono,
                    fontSize = 14.sp,
                    modifier = Modifier.clickable { viewModel.reset(); onRegister() },
                )
            }
            Spacer(Modifier.height(32.dp))
            // 自建服务器的人在这里改地址；默认值就是构建时固定的那个
            val configured = serverBase
            Text(
                text = if (configured == null) {
                    // 构建时没注入默认地址（解耦后的正常情况）：提示用户自己填
                    if (BuildConfig.BASE_URL.isBlank()) {
                        stringResource(R.string.server_not_set)
                    } else {
                        stringResource(R.string.server_using_default, hostOf(BuildConfig.BASE_URL))
                    }
                } else {
                    stringResource(R.string.server_current, hostOf(configured))
                },
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.clickable { showServerDialog = true },
            )
        }
    }

    if (showServerDialog) {
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
                    // release 的网络安全策略只放 https，填 http 会被系统拦掉，先说清楚
                    !BuildConfig.DEBUG && normalized.startsWith("http://") ->
                        context.getString(R.string.server_address_https_required)
                    else -> null
                }
                if (serverError == null) {
                    ServerConfigStore.write(context, normalized)
                    serverBase = normalized
                    showServerDialog = false
                }
            },
            onDismiss = { showServerDialog = false; serverError = null },
        )
    }
}

@Composable
fun CodeScreen(
    email: String,
    name: String?,
    onVerified: (String, Boolean) -> Unit,
    onBack: () -> Unit,
    viewModel: AuthViewModel = hiltViewModel(),
) {
    var code by remember { mutableStateOf("") }
    val state by viewModel.state.collectAsStateWithLifecycle()

    LaunchedEffect(state) {
        // 只有新账号才去欢迎页；老账号直接进对话列表，不要再看「欢迎加入」
        (state as? AuthUiState.Done)?.let { onVerified(it.account.name, it.newAccount) }
    }

    ScreenRoot {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(horizontal = ImboDimen.Gutter),
            verticalArrangement = Arrangement.Center,
        ) {
            Text(
                text = stringResource(R.string.check_email_title),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 22.sp,
                lineHeight = 28.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                text = stringResource(R.string.check_email_body, email.ifBlank { "you@example.com" }),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                lineHeight = 25.sp,
            )
            Spacer(Modifier.height(24.dp))
            InsetField(
                label = stringResource(R.string.code_label),
                value = code,
                placeholder = "000000",
                onValueChange = { if (it.length <= 6 && it.all { c -> c.isDigit() }) { code = it; if (state is AuthUiState.Failed) viewModel.reset() } },
                keyboardType = KeyboardType.NumberPassword,
            )
            Spacer(Modifier.height(24.dp))
            if (state is AuthUiState.Failed) {
                Text(
                    text = authErrorText((state as AuthUiState.Failed).message),
                    color = ImboColor.Error,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                )
                Spacer(Modifier.height(12.dp))
            }
            PrimaryButton(
                text = if (state == AuthUiState.Working) stringResource(R.string.verifying) else stringResource(R.string.verify_action),
                onClick = { viewModel.verify(email, code) },
                enabled = code.length == 6 && state != AuthUiState.Working,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(24.dp))
            // 服务端 60 秒才允许再发一次码；这里跟着倒计时，别让用户点了才看到「太频繁」
            var secondsLeft by remember { mutableStateOf(60) }
            LaunchedEffect(Unit) {
                while (secondsLeft > 0) {
                    kotlinx.coroutines.delay(1000)
                    secondsLeft -= 1
                }
            }
            Text(
                text = if (secondsLeft > 0) {
                    stringResource(R.string.resend_code_countdown, secondsLeft)
                } else {
                    stringResource(R.string.resend_code)
                },
                color = if (secondsLeft > 0) ImboColor.Tertiary else ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                modifier = Modifier.clickable(enabled = secondsLeft == 0) {
                    viewModel.requestCode(email, name)
                    secondsLeft = 60
                },
            )
            Spacer(Modifier.height(8.dp))
            Text(
                text = stringResource(R.string.back),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                modifier = Modifier.clickable { viewModel.reset(); onBack() },
            )
        }
    }
}

@Composable
fun RegisterScreen(
    onCodeSent: (String, String) -> Unit,
    onSignIn: () -> Unit,
    viewModel: AuthViewModel = hiltViewModel(),
) {
    var name by remember { mutableStateOf("") }
    var email by remember { mutableStateOf("") }
    val state by viewModel.state.collectAsStateWithLifecycle()

    LaunchedEffect(state) {
        (state as? AuthUiState.CodeSent)?.let { onCodeSent(it.email, name) }
    }

    ScreenRoot {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(horizontal = ImboDimen.Gutter),
            verticalArrangement = Arrangement.Center,
        ) {
            Text(
                text = stringResource(R.string.register_title),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 22.sp,
                lineHeight = 28.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                text = stringResource(R.string.register_body),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                lineHeight = 25.sp,
            )
            Spacer(Modifier.height(24.dp))
            InsetField(
                label = stringResource(R.string.name_label),
                value = name,
                placeholder = "alice",
                onValueChange = { name = it },
            )
            Spacer(Modifier.height(16.dp))
            InsetField(
                label = stringResource(R.string.email_label),
                value = email,
                placeholder = stringResource(R.string.email_placeholder),
                onValueChange = { email = it.trim(); if (state is AuthUiState.Failed) viewModel.reset() },
                keyboardType = KeyboardType.Email,
            )
            Spacer(Modifier.height(24.dp))
            if (state is AuthUiState.Failed) {
                Text(
                    text = authErrorText((state as AuthUiState.Failed).message),
                    color = ImboColor.Error,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                )
                Spacer(Modifier.height(12.dp))
            }
            PrimaryButton(
                text = if (state == AuthUiState.Working) stringResource(R.string.sending) else stringResource(R.string.create_account),
                onClick = { viewModel.requestCode(email, name) },
                enabled = name.isNotBlank() && email.contains("@") && state != AuthUiState.Working,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(40.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(R.string.have_account), color = ImboColor.Tertiary, fontFamily = ImboMono, fontSize = 14.sp)
                HSpacer(4)
                Text(
                    text = stringResource(R.string.sign_in_link),
                    color = ImboColor.OnBackground,
                    fontFamily = ImboMono,
                    fontSize = 14.sp,
                    modifier = Modifier.clickable { viewModel.reset(); onSignIn() },
                )
            }
        }
    }
}

@Composable
fun WelcomeScreen(name: String, onOpen: () -> Unit) {
    ScreenRoot {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(horizontal = ImboDimen.Gutter),
            verticalArrangement = Arrangement.Center,
        ) {
            Wordmark()
            Spacer(Modifier.height(32.dp))
            Text(
                text = stringResource(R.string.welcome_title, name.ifBlank { "alice" }),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 22.sp,
                lineHeight = 28.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                text = stringResource(R.string.welcome_body),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                lineHeight = 25.sp,
            )
            Spacer(Modifier.height(32.dp))
            PrimaryButton(
                text = stringResource(R.string.open_imbo),
                onClick = onOpen,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}
