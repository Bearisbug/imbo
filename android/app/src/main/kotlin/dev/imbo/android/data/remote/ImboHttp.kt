package dev.imbo.android.data.remote

import dev.imbo.android.data.SessionState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.DeserializationStrategy
import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import java.util.concurrent.TimeUnit

/** 服务端返回的错误（RFC 7807）。401 需要上层清登录态。 */
class ImboApiException(
    val status: Int,
    val code: String,
    override val message: String,
) : Exception(message)

/**
 * 一层薄 HTTP：加 Authorization、序列化、把非 2xx 转成 ImboApiException。
 * 不做重试和缓存，重试由 ViewModel 的显式调用决定。
 */
class ImboHttp(
    private val defaultBaseUrl: String,
    private val store: SessionState,
    /** debug 构建下可以覆盖服务端地址（登录页长按 wordmark 设置），release 恒为 null。 */
    private val overrideBase: () -> String? = { null },
    /** 收到 401 时回调，用来清登录态并回登录页。 */
    private val onUnauthorized: () -> Unit = {},
    val client: OkHttpClient = defaultClient(),
) {
    val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
        encodeDefaults = true
    }

    /** 每次请求现取，改地址后不用重启进程。 */
    val base: String get() = (overrideBase() ?: defaultBaseUrl).trimEnd('/')

    suspend fun <T> get(path: String, serializer: DeserializationStrategy<T>): T =
        decode(perform(Request.Builder().url(base + path).get()), serializer)

    suspend fun <T> post(path: String, body: String?, serializer: DeserializationStrategy<T>): T =
        decode(perform(Request.Builder().url(base + path).post(jsonBody(body))), serializer)

    /** multipart 上传：Content-Type 由调用方构造的 body 决定，其余头与普通请求一致。 */
    suspend fun <T> postMultipart(path: String, body: okhttp3.RequestBody, serializer: DeserializationStrategy<T>): T =
        decode(perform(Request.Builder().url(base + path).post(body)), serializer)

    /**
     * 上传附件。服务端 /v1/files 读的是原始请求体，不是 multipart：
     * 文件名放 Imbo-File-Name（百分号编码），内容 SHA-256 放 Imbo-File-Sha256（64 位小写十六进制）。
     */
    suspend fun <T> postRawFile(
        path: String,
        bytes: ByteArray,
        fileName: String,
        sha256Hex: String,
        contentType: String,
        serializer: DeserializationStrategy<T>,
    ): T = decode(
        perform(
            Request.Builder()
                .url(base + path)
                .header("Imbo-File-Name", percentEncode(fileName))
                .header("Imbo-File-Sha256", sha256Hex)
                .post(
                    bytes.toRequestBody(
                        runCatching { contentType.toMediaType() }
                            .getOrNull() ?: "application/octet-stream".toMediaType(),
                    ),
                ),
        ),
        serializer,
    )

    /**
     * 上传二进制（头像）。服务端 /v1/me/avatar 直接读原始请求体，
     * 并要求 Content-Type 是 image/jpeg 或 image/png，不走 multipart。
     */
    suspend fun <T> putBytes(
        path: String,
        bytes: ByteArray,
        contentType: String,
        serializer: DeserializationStrategy<T>,
    ): T = decode(
        perform(Request.Builder().url(base + path).put(bytes.toRequestBody(contentType.toMediaType()))),
        serializer,
    )

    suspend fun <T> put(path: String, body: String?, serializer: DeserializationStrategy<T>): T =
        decode(perform(Request.Builder().url(base + path).put(jsonBody(body))), serializer)

    suspend fun <T> patch(path: String, body: String?, serializer: DeserializationStrategy<T>): T =
        decode(perform(Request.Builder().url(base + path).patch(jsonBody(body))), serializer)

    suspend fun getUnit(path: String) = perform(Request.Builder().url(base + path).get())

    /** 下载二进制（附件）。响应体按字节读，不走 JSON 解码。 */
    suspend fun getBytes(path: String): ByteArray = withContext(Dispatchers.IO) {
        val builder = Request.Builder().url(base + path).get()
        store.token?.let { builder.header("Authorization", "Bearer $it") }
        client.newCall(builder.build()).execute().use { res ->
            if (!res.isSuccessful) {
                throw ImboApiException(res.code, "download_failed", "HTTP ${res.code}")
            }
            res.body?.bytes() ?: ByteArray(0)
        }
    }

    suspend fun postUnit(path: String, body: String? = null) =
        perform(Request.Builder().url(base + path).post(jsonBody(body ?: "{}")))

    suspend fun putUnit(path: String, body: String?) =
        perform(Request.Builder().url(base + path).put(jsonBody(body)))

    suspend fun deleteUnit(path: String) =
        perform(Request.Builder().url(base + path).delete())

    suspend fun deleteUnit(path: String, body: String?) =
        perform(Request.Builder().url(base + path).delete(jsonBody(body)))

    private fun jsonBody(body: String?): okhttp3.RequestBody =
        (body ?: "").toRequestBody(JSON)

    private suspend fun <T> decode(text: String, serializer: DeserializationStrategy<T>): T =
        json.decodeFromString(serializer, text)

    /** 发请求并把非 2xx 变成异常，返回原始响应文本。 */
    private suspend fun perform(builder: Request.Builder): String = withContext(Dispatchers.IO) {
        store.token?.let { builder.header("Authorization", "Bearer $it") }
        builder.header("Accept", "application/json")

        client.newCall(builder.build()).execute().use { res ->
            val text = res.body?.string().orEmpty()
            if (!res.isSuccessful) {
                // token 失效：清掉本地登录态并通知上层回登录页；登录接口本身没有 token，不触发
                if (res.code == 401 && store.token != null) {
                    store.clear()
                    onUnauthorized()
                }
                val problem = runCatching { json.decodeFromString(ProblemDto.serializer(), text) }.getOrNull()
                val detail = problem?.detail?.takeIf { it.isNotBlank() }
                    ?: problem?.title?.takeIf { it.isNotBlank() }
                    ?: "HTTP ${res.code}"
                throw ImboApiException(res.code, problem?.type.orEmpty(), detail)
            }
            text
        }
    }

    companion object {
        private val JSON = "application/json; charset=utf-8".toMediaType()

        /**
         * 百分号编码：只保留 RFC 3986 的 unreserved 字符，空格编成 %20 而不是 +，
         * 因为服务端是按 percent_decode 解析的。
         */
        fun percentEncode(value: String): String {
            val out = StringBuilder()
            for (b in value.toByteArray(Charsets.UTF_8)) {
                val c = b.toInt() and 0xFF
                val unreserved = c in 0x30..0x39 || c in 0x41..0x5A || c in 0x61..0x7A ||
                    c == 0x2D || c == 0x2E || c == 0x5F || c == 0x7E
                if (unreserved) out.append(c.toChar()) else out.append('%').append("%02X".format(c))
            }
            return out.toString()
        }

        fun defaultClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(30, TimeUnit.SECONDS)
            .pingInterval(25, TimeUnit.SECONDS)
            .build()
    }
}
