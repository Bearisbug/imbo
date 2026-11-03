package dev.imbo.android.data

import android.content.Context
import dagger.hilt.android.qualifiers.ApplicationContext
import javax.inject.Inject
import javax.inject.Singleton

/**
 * 登录态的读写口。真实实现落 SharedPreferences，单测用内存实现，
 * 这样 HTTP 客户端可以脱离 Android 上下文测试。
 */
interface SessionState {
    var token: String?

    /** 0 表示还没登录过。 */
    var accountId: Int

    val signedIn: Boolean

    fun clear()
}

/**
 * token 只在登录响应里出现一次（服务端只存 SHA-256），所以必须落盘。
 */
@Singleton
class SessionStore @Inject constructor(@ApplicationContext context: Context) : SessionState {

    private val prefs = context.getSharedPreferences("imbo_session", Context.MODE_PRIVATE)

    override var token: String?
        get() = prefs.getString(KEY_TOKEN, null)
        set(value) = prefs.edit().apply {
            if (value == null) remove(KEY_TOKEN) else putString(KEY_TOKEN, value)
        }.apply()

    override var accountId: Int
        get() = prefs.getInt(KEY_ACCOUNT, 0)
        set(value) = prefs.edit().putInt(KEY_ACCOUNT, value).apply()

    override val signedIn: Boolean get() = !token.isNullOrBlank()

    override fun clear() {
        prefs.edit().clear().apply()
    }

    private companion object {
        const val KEY_TOKEN = "token"
        const val KEY_ACCOUNT = "account_id"
    }
}

/** 单测与预览用的内存实现。 */
class InMemorySessionState(
    override var token: String? = null,
    override var accountId: Int = 0,
) : SessionState {
    override val signedIn: Boolean get() = !token.isNullOrBlank()

    override fun clear() {
        token = null
        accountId = 0
    }
}
