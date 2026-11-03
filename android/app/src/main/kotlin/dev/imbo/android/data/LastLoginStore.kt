package dev.imbo.android.data

import android.content.Context

/**
 * 记一下上次用来登录的邮箱，下次打开登录页直接填好。
 * 只是省一次输入，不含任何凭据；退出登录不会清掉，换账号时会被覆盖。
 */
object LastLoginStore {
    private const val PREFS = "imbo_last_login"
    private const val KEY = "email"

    fun read(context: Context): String? = context
        .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        .getString(KEY, null)
        ?.trim()
        ?.takeIf { it.isNotBlank() }

    fun write(context: Context, email: String) {
        val trimmed = email.trim()
        if (trimmed.isEmpty()) return
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY, trimmed)
            .apply()
    }
}
