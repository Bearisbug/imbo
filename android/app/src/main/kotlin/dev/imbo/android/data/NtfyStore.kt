package dev.imbo.android.data

import android.content.Context
import dev.imbo.android.BuildConfig
import android.provider.Settings
import java.security.MessageDigest

/**
 * ntfy 话题：这台手机的通知地址。
 *
 * 由 `ANDROID_ID`（同一签名+同一设备稳定）+ 包名派生，所以重装以后话题不变——
 * ntfy 客户端订阅一次就长期有效。首次生成后缓存到偏好里，之后直接读。
 *
 * 话题名就是密码：知道的人都能读到通知。用 ANDROID_ID 派生而不是随机串，
 * 是因为重装后随机串会变，得让用户重新订阅一次。
 */
object NtfyStore {
    private const val PREFS = "imbo_ntfy"
    private const val KEY_TOPIC = "topic"
    private const val KEY_BASE = "base_url"

    fun topic(context: Context): String {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        prefs.getString(KEY_TOPIC, null)?.takeIf { it.isNotBlank() }?.let { return it }
        val androidId = runCatching {
            Settings.Secure.getString(context.contentResolver, Settings.Secure.ANDROID_ID)
        }.getOrNull().orEmpty()
        val topic = topicFrom(androidId, context.packageName)
        prefs.edit().putString(KEY_TOPIC, topic).apply()
        return topic
    }

    /**
     * 纯函数版：同样的设备 + 包名永远得到同一个话题，重装不影响。
     * `androidId` 为空（拿不到）时退回随机串，至少能用。
     */
    fun topicFrom(androidId: String, packageName: String): String {
        if (androidId.isBlank()) {
            val alphabet = "abcdefghijklmnopqrstuvwxyz0123456789"
            val random = java.security.SecureRandom()
            return "imbo-" + (1..12).map { alphabet[random.nextInt(alphabet.length)] }.joinToString("")
        }
        val digest = MessageDigest.getInstance("SHA-256")
            .digest("$androidId/$packageName".toByteArray())
        return "imbo-" + digest.joinToString("") { "%02x".format(it) }.take(12)
    }

    /** 用户在账号页填的 ntfy 基础地址；没填就回退到构建时注入的那个（可空）。 */
    fun baseUrl(context: Context): String? = context
        .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        .getString(KEY_BASE, null)
        ?.trim()
        ?.trimEnd('/')
        ?.takeIf { it.isNotBlank() }

    /** 账号页改 ntfy 地址用；传空就清除，回到构建时的默认值。 */
    fun writeBaseUrl(context: Context, value: String?) {
        val editor = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
        val trimmed = value?.trim()?.trimEnd('/')?.takeIf { it.isNotBlank() }
        if (trimmed == null) editor.remove(KEY_BASE) else editor.putString(KEY_BASE, trimmed)
        editor.apply()
    }

    /** 订阅地址 = 用户填的地址 / 构建时注入的地址 + 本机话题；都没有就返回 null。 */
    fun subscribeUrl(context: Context): String? {
        val base = baseUrl(context) ?: BuildConfig.NTFY_URL.trim().trimEnd('/')
        if (base.isBlank()) return null
        return "$base/${topic(context)}"
    }
}
