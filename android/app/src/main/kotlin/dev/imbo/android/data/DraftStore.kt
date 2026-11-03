package dev.imbo.android.data

import android.content.Context
import dagger.hilt.android.qualifiers.ApplicationContext
import javax.inject.Inject
import javax.inject.Singleton

/**
 * 未发送的草稿，按对话 id 存。
 * 不用 SavedStateHandle 是因为它跟着返回栈一起销毁，退出对话再回来就没了。
 */
interface DraftStorage {
    fun read(conversationId: Long): String
    fun write(conversationId: Long, text: String)
    fun clear(conversationId: Long)
}

@Singleton
class PrefsDraftStore @Inject constructor(@ApplicationContext context: Context) : DraftStorage {

    private val prefs = context.getSharedPreferences("imbo_drafts", Context.MODE_PRIVATE)

    private fun key(id: Long) = "chat_draft_$id"

    override fun read(conversationId: Long): String =
        prefs.getString(key(conversationId), null).orEmpty()

    override fun write(conversationId: Long, text: String) {
        if (conversationId == 0L) return
        prefs.edit().apply {
            if (text.isBlank()) remove(key(conversationId)) else putString(key(conversationId), text)
        }.apply()
    }

    override fun clear(conversationId: Long) = write(conversationId, "")
}

/** 单测用。 */
class InMemoryDraftStorage : DraftStorage {
    private val drafts = mutableMapOf<Long, String>()

    override fun read(conversationId: Long): String = drafts[conversationId].orEmpty()

    override fun write(conversationId: Long, text: String) {
        if (text.isBlank()) drafts.remove(conversationId) else drafts[conversationId] = text
    }

    override fun clear(conversationId: Long) {
        drafts.remove(conversationId)
    }
}
