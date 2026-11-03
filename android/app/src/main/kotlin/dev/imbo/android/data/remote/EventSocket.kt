package dev.imbo.android.data.remote

import dev.imbo.android.data.ChangeArea
import dev.imbo.android.data.SessionState
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.channelFlow
import kotlinx.coroutines.isActive
import kotlinx.serialization.Serializable
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import javax.inject.Inject
import javax.inject.Singleton

@Serializable
private data class EventDto(val type: String = "", val area: String? = null)

/**
 * /v1/events 只推「某个区域变了」。这里把它翻译成 ChangeArea，
 * 界面收到后自己重新拉取，不做本地补丁。
 */
@Singleton
class EventSocket @Inject constructor(
    private val http: ImboHttp,
    private val store: SessionState,
) {
    val areas: Flow<ChangeArea> = channelFlow {
        var backoff = 1_000L
        while (isActive && !isClosedForSend) {
            val closed = CompletableDeferred<Unit>()
            val request = Request.Builder()
                .url(wsUrl())
                .apply { store.token?.let { addHeader("Authorization", "Bearer $it") } }
                .build()

            val socket = http.client.newWebSocket(request, object : WebSocketListener() {
                override fun onMessage(webSocket: WebSocket, text: String) {
                    trySend(parseArea(text))
                }

                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    webSocket.close(1000, null)
                    closed.complete(Unit)
                }

                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                    closed.complete(Unit)
                }

                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                    closed.complete(Unit)
                }
            })

            closed.await()
            socket.cancel()
            delay(backoff)
            backoff = (backoff * 2).coerceAtMost(30_000L)
        }
        awaitClose { }
    }

    private fun wsUrl(): String = http.base
        .replaceFirst("https://", "wss://")
        .replaceFirst("http://", "ws://") + "/v1/events"

    private fun parseArea(text: String): ChangeArea {
        val dto = runCatching { http.json.decodeFromString(EventDto.serializer(), text) }.getOrNull()
        if (dto?.type != "changed") return ChangeArea.All
        return when (dto.area) {
            "conversations" -> ChangeArea.Conversations
            "messages" -> ChangeArea.Messages
            "agents" -> ChangeArea.Agents
            "contacts" -> ChangeArea.Contacts
            "requests" -> ChangeArea.Requests
            "billing" -> ChangeArea.Billing
            // 未知区域按全量刷新处理，宁可多拉一次也不要漏
            else -> ChangeArea.All
        }
    }
}
