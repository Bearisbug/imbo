package dev.imbo.android.ui.components

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import dev.imbo.android.R

/**
 * 服务端返回的错误详情是英文（RFC 7807 的 detail），直接显示在中文界面里很突兀。
 *
 * 这里按已知文案做一次映射，命中就换成本地化说法，没命中就原样显示——
 * 宁可露英文，也不要把「为什么失败」吞掉。
 *
 * 拆成「纯函数查表 + 组合函数取字符串」两层：查表不需要 Android 上下文，可以直接写单测。
 */
fun serverErrorRes(message: String?): Int? {
    val raw = message?.takeIf { it.isNotBlank() } ?: return null
    return when {
        raw.contains("already your contact", ignoreCase = true) -> R.string.err_already_contact
        raw.contains("not your contact", ignoreCase = true) -> R.string.err_not_contact
        raw.contains("signed out", ignoreCase = true) ||
            raw.contains("not signed in", ignoreCase = true) -> R.string.err_signed_out
        raw.contains("wrong or expired", ignoreCase = true) -> R.string.auth_err_code
        raw.contains("must be an expo", ignoreCase = true) -> R.string.err_push_token
        raw.contains("rate limited", ignoreCase = true) ||
            raw.contains("too many requests", ignoreCase = true) ||
            raw.contains("just sent", ignoreCase = true) ||
            raw.contains("wait a minute", ignoreCase = true) -> R.string.err_rate_limited
        raw.contains("is not in", ignoreCase = true) && raw.contains("invite", ignoreCase = true) ->
            R.string.err_group_not_invited
        raw.contains("only the owner", ignoreCase = true) -> R.string.err_not_owner
        raw.contains("can't be empty", ignoreCase = true) ||
            raw.contains("can not be empty", ignoreCase = true) -> R.string.err_empty_value
        // OkHttp 的本地异常文案：连不上、解析不了域名、连接被断，这几种最常见
        raw.contains("failed to connect", ignoreCase = true) ||
            raw.contains("connectexception", ignoreCase = true) ||
            raw.contains("unable to resolve host", ignoreCase = true) ||
            raw.contains("connection refused", ignoreCase = true) ||
            raw.contains("unexpected end of stream", ignoreCase = true) ||
            raw.contains("connection reset", ignoreCase = true) ||
            raw.contains("broken pipe", ignoreCase = true) -> R.string.err_network
        raw.contains("timeout", ignoreCase = true) || raw.contains("timed out", ignoreCase = true) ->
            R.string.err_network_timeout
        else -> null
    }
}

/** 命中映射就用本地化文案，否则原样返回（可能为 null，调用方决定兜底）。 */
@Composable
fun localizedServerError(message: String?): String? {
    val res = serverErrorRes(message) ?: return message?.takeIf { it.isNotBlank() }
    return stringResource(res)
}
