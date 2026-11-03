package dev.imbo.android.data

import android.content.Context
import android.content.res.Configuration
import java.util.Locale

/**
 * 应用内语言：不改系统设置，只在本应用生效。
 * 写入后由 Activity 的 attachBaseContext 换掉 configuration，再 recreate 生效。
 */
object LocaleStore {
    private const val PREFS = "imbo_prefs"
    private const val KEY = "app_language"

    /** null = 跟随系统。 */
    fun read(context: Context): String? = context
        .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        .getString(KEY, null)
        ?.takeIf { it.isNotBlank() }

    fun write(context: Context, tag: String?) {
        val editor = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
        if (tag.isNullOrBlank()) editor.remove(KEY) else editor.putString(KEY, tag)
        editor.apply()
    }

    /** 跟随系统时直接返回原 context。 */
    fun wrap(base: Context): Context {
        val tag = read(base) ?: return base
        val locale = Locale.forLanguageTag(tag)
        Locale.setDefault(locale)
        val config = Configuration(base.resources.configuration)
        config.setLocale(locale)
        return base.createConfigurationContext(config)
    }
}
