package dev.imbo.android.push

import android.content.Context
import dagger.hilt.android.qualifiers.ApplicationContext
import dev.imbo.android.BuildConfig
import dev.imbo.android.data.ImboApi
import dev.imbo.android.data.LocaleStore
import dev.imbo.android.data.NtfyStore
import java.util.Locale
import javax.inject.Inject
import javax.inject.Singleton

/**
 * 把本机的 ntfy 话题注册给服务端，并保持「登录才注册、退出就注销」。
 *
 * 通知本身由 ntfy 客户端投递（服务端 POST 到话题，客户端订阅话题），
 * App 不需要常驻连接；没装 ntfy 客户端时只是收不到通知，发消息不受影响。
 */
@Singleton
class PushRegistrar @Inject constructor(
    private val api: ImboApi,
    @ApplicationContext private val context: Context,
) {
    val enabled: Boolean get() = !BuildConfig.USE_MOCK

    /** 登录成功后调用：把本机话题交给服务端。 */
    suspend fun register() {
        if (!enabled) return
        val topic = NtfyStore.topic(context)
        val result = runCatching { api.registerPush("ntfy:$topic", pushLocale()) }
        android.util.Log.i(
            "ImboPush",
            "register ntfy topic=${topic.take(8)}… ok=${result.isSuccess} ${result.exceptionOrNull()?.message.orEmpty()}",
        )
    }

    /** 话题换了（重装/清数据）时用；当前话题固定，保留接口给以后的多话题。 */
    suspend fun registerWith(token: String) {
        if (!enabled) return
        runCatching { api.registerPush(token, pushLocale()) }
    }

    /** 退出登录 / 删除账号时调用，别让这台手机继续收旧账号的消息。 */
    suspend fun unregister() {
        if (!enabled) return
        runCatching { api.unregisterPush() }
    }

    /** 服务端只认 en 与 zh-Hans，用来拼「@了你」这类提示的文案。 */
    private fun pushLocale(): String {
        val tag = LocaleStore.read(context) ?: Locale.getDefault().language
        return if (tag.startsWith("zh")) "zh-Hans" else "en"
    }
}
