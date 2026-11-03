package dev.imbo.android.data

import android.content.Context

/**
 * 服务端地址：存在 prefs 里，release 也能改，换服务器不用重新构建。
 * 空值表示「用构建时的 BuildConfig.BASE_URL」，所以老包的行为不变。
 *
 * 只存 base（scheme + host + 可选端口/路径前缀），不存 token：
 * 换服务器后 token 属于旧服务器，必须重新登录。
 */
object ServerConfigStore {
    private const val PREFS = "imbo_server"
    private const val KEY = "base_url"

    fun read(context: Context): String? = context
        .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        .getString(KEY, null)
        ?.trim()
        ?.takeIf { it.isNotBlank() }

    fun write(context: Context, value: String?) {
        val editor = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
        val normalized = value?.let { normalize(it) }
        if (normalized.isNullOrBlank()) editor.remove(KEY) else editor.putString(KEY, normalized)
        editor.apply()
    }

    fun clear(context: Context) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().remove(KEY).apply()
    }

    /** 界面上只想显示主机部分，完整地址留在对话框里。 */
    fun displayHost(base: String): String =
        normalize(base)?.substringAfter("://").orEmpty()

    /**
     * 把用户输入整理成可用的 base：
     * - 没写协议就补 https（手机上手输地址几乎没人会写 scheme）
     * - 去掉尾部斜杠，避免拼出 //v1/health
     * - 拒绝空白、查询串、片段；路径前缀保留（反代挂在子路径时要用）
     */
    fun normalize(raw: String): String? {
        val trimmed = raw.trim()
        if (trimmed.isEmpty()) return null
        if (trimmed.any { it.isWhitespace() }) return null
        if (trimmed.contains('?') || trimmed.contains('#')) return null

        val withScheme = if (trimmed.contains("://")) trimmed else "https://$trimmed"
        val scheme = withScheme.substringBefore("://").lowercase()
        if (scheme != "http" && scheme != "https") return null

        val rest = withScheme.substringAfter("://").trimEnd('/')
        if (rest.isEmpty()) return null
        // host 必须像主机名或 IP，不能只有端口
        val host = rest.substringBefore('/').substringBefore(':')
        if (host.isEmpty() || host.contains(' ')) return null

        return "$scheme://$rest"
    }
}
