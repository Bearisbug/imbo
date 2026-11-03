package dev.imbo.android

import android.content.Context
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateOf
import androidx.lifecycle.lifecycleScope
import dagger.hilt.android.AndroidEntryPoint
import dev.imbo.android.data.LocaleStore
import dev.imbo.android.push.Notifications
import dev.imbo.android.push.PushRegistrar
import dev.imbo.android.ui.nav.ImboNavHost
import dev.imbo.android.ui.nav.Routes
import dev.imbo.android.ui.theme.ImboColor
import dev.imbo.android.ui.theme.ImboTheme
import kotlinx.coroutines.launch

@AndroidEntryPoint
class MainActivity : ComponentActivity() {

    @javax.inject.Inject
    lateinit var mockConfig: kotlinx.coroutines.flow.MutableStateFlow<dev.imbo.android.data.MockConfig>

    @javax.inject.Inject
    lateinit var sessionEvents: dev.imbo.android.data.SessionEvents

    @javax.inject.Inject
    lateinit var sessionStore: dev.imbo.android.data.SessionStore

    @javax.inject.Inject
    lateinit var push: PushRegistrar

    /** 点通知进来要打开的会话；null 表示没有待处理的通知。 */
    private var pendingConversation = mutableStateOf<Long?>(null)

    /** 应用内语言在这里生效，跟随系统时是空操作。 */
    override fun attachBaseContext(newBase: Context) {
        super.attachBaseContext(LocaleStore.wrap(newBase))
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        applyMockScenario()
        pendingConversation.value = conversationFrom(intent)
        // 已经有登录态就顺手把推送令牌注册上；登录成功后的那次由会话变化触发
        lifecycleScope.launch { if (sessionStore.signedIn) push.register() }
        setContent {
            ImboTheme {
                // 验收用：adb 传 --es route chats 之类可直接落到指定屏，方便逐屏截图。
                val debugRoute = if (BuildConfig.DEBUG) intent?.getStringExtra("route") else null
                // 已经登录过就直接进对话列表，不必每次冷启动都重新收验证码
                val startRoute = debugRoute ?: if (sessionStore.signedIn) {
                    Routes.CHATS
                } else {
                    null
                }
                val navController = androidx.navigation.compose.rememberNavController()
                // 通知点进来的会话：等已登录再跳，否则先留在登录页
                LaunchedEffect(pendingConversation.value) {
                    val id = pendingConversation.value ?: return@LaunchedEffect
                    pendingConversation.value = null
                    if (sessionStore.signedIn && id > 0) navController.navigate(Routes.chat(id))
                }
                // 服务端返回 401 时回登录页并清空返回栈，避免退回到已失效的页面
                androidx.compose.runtime.LaunchedEffect(Unit) {
                    sessionEvents.signedOut.collect {
                        push.unregister()
                        navController.navigate(Routes.LOGIN) {
                            popUpTo(0) { inclusive = true }
                        }
                    }
                }
                androidx.compose.material3.Surface(
                    color = ImboColor.Background,
                    content = { ImboNavHost(navController = navController, startRoute = startRoute) },
                )
            }
        }
    }

    /** singleTop 下点第二、第三条通知走这里，而不是重建 Activity。 */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        pendingConversation.value = conversationFrom(intent)
    }

    /**
     * 要打开的会话来自两处：通知的 conversation_id 额外字段，或 `imbo://chat/<id>` 深链
     * （ntfy 的 click 用它，点通知能直接落到会话）。
     */
    private fun conversationFrom(intent: Intent?): Long? {
        val raw = intent?.getStringExtra(Notifications.EXTRA_CONVERSATION)
        raw?.toLongOrNull()?.takeIf { it > 0 }?.let { return it }
        val data = intent?.data ?: return null
        if (data.scheme != "imbo" || data.host != "chat") return null
        return data.pathSegments.firstOrNull()?.toLongOrNull()?.takeIf { it > 0 }
    }

    /** 验收用：--es mock_error 401 --es mock_latency 1500 --ez mock_empty true */
    private fun applyMockScenario() {
        if (!BuildConfig.DEBUG) return
        val i = intent ?: return
        val empty = i.getBooleanExtra("mock_empty", false)
        val error = when (i.getStringExtra("mock_error")) {
            "401" -> dev.imbo.android.data.ForceError.Unauthorized
            "500" -> dev.imbo.android.data.ForceError.ServerError
            "timeout" -> dev.imbo.android.data.ForceError.Timeout
            else -> null
        }
        val latency = i.getStringExtra("mock_latency")?.toLongOrNull()
        if (empty || error != null || latency != null) {
            mockConfig.value = mockConfig.value.copy(
                emptyData = empty,
                forceError = error,
                latencyMs = latency ?: mockConfig.value.latencyMs,
            )
        }
    }
}
