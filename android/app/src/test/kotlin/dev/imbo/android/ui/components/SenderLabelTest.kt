package dev.imbo.android.ui.components

import dev.imbo.android.data.Message
import org.junit.Assert.assertEquals
import org.junit.Test

class SenderLabelTest {
    private fun message(name: String, short: String?, tool: String? = null) = Message(
        id = 1,
        conversationId = 1,
        body = "",
        senderName = name,
        mine = false,
        createdAt = 0,
        senderAgentShort = short,
        senderAgentTool = tool,
    )

    @Test
    fun `unique title stays bare`() {
        assertEquals("codex", senderLabel(message("codex", "1a3cb5"), emptySet()))
    }

    @Test
    fun `a duplicate title still shows the tool, not the id`() {
        assertEquals("codex · dsh", senderLabel(message("codex", "1a3cb5", "dsh"), setOf("codex")))
    }

    @Test
    fun `a person never gets a suffix`() {
        assertEquals("bob", senderLabel(message("bob", null), setOf("bob")))
    }

    @Test
    fun `an agent message shows its tool`() {
        assertEquals("codex · dsh", senderLabel(message("codex", "1a3cb5", "dsh"), emptySet()))
    }
}
