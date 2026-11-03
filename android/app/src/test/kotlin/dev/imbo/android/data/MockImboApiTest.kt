package dev.imbo.android.data

import dev.imbo.android.data.mock.MockImboApi
import dev.imbo.android.data.mock.MockException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * mock 通道是验收用的唯一数据源，这里锁住它的行为，
 * 免得改界面时把「5 条未读」「拉黑后消失」这类事实改坏。
 */
class MockImboApiTest {

    private fun api(config: MockConfig = noLatency()) = MockImboApi(MutableStateFlow(config))

    @Test
    fun `conversations carry four rows and five unread`() = runTest {
        val conversations = api().conversations()
        assertEquals(4, conversations.size)
        assertEquals(5, conversations.sumOf { it.pending })
        assertEquals("bob", conversations.first().peer?.name)
    }

    @Test
    fun `agent session stays out of the chat list but keeps unread`() = runTest {
        val api = api()
        assertFalse(api.conversations().any { it.id == 5L })
        assertEquals(2, api.agents().sumOf { it.pending })
    }

    @Test
    fun `opening an agent session clears its unread`() = runTest {
        val api = api()
        assertEquals(1, api.agents().first { it.id == "agent-checkout" }.pending)
        api.markRead("agent-checkout")
        assertEquals(0, api.agents().first { it.id == "agent-checkout" }.pending)
    }

    @Test
    fun `sending a message stores it as mine with a client id`() = runTest {
        val api = api(MockConfig(latencyMs = 0, fakeReply = false))
        val before = api.messages(1L).messages.size
        val sent = api.sendMessage(1L, "ping", "client-1")

        assertTrue(sent.mine)
        assertEquals("client-1", sent.clientId)
        val after = api.messages(1L).messages
        assertEquals(before + 1, after.size)
        assertEquals("ping", after.last().body)
    }

    @Test
    fun `uploading keeps the local uri and sending stores the attachment`() = runTest {
        val api = api(MockConfig(latencyMs = 0, fakeReply = false))
        val uploaded = api.uploadAttachment("content://media/photo.jpg", "photo.jpg", "image/jpeg")

        assertEquals("content://media/photo.jpg", uploaded.localUri)
        assertEquals("photo.jpg", uploaded.name)

        val sent = api.sendMessage(1L, "", "client-attach", listOf(uploaded))

        assertEquals(listOf("photo.jpg"), sent.attachments.map { it.name })
        assertEquals(uploaded.localUri, api.messages(1L).messages.last().attachments.single().localUri)
    }

    @Test
    fun `blocking hides the conversation and the contact`() = runTest {
        val api = api()
        assertTrue(api.conversations().any { it.peer?.id == "acc-bob" })

        api.block("acc-bob")

        assertFalse(api.conversations().any { it.peer?.id == "acc-bob" })
        assertFalse(api.contacts().any { it.id == "acc-bob" })
        assertEquals(listOf("bob"), api.blocked().map { it.name })
    }

    @Test
    fun `unblocking restores both`() = runTest {
        val api = api()
        api.block("acc-bob")
        api.unblock("acc-bob")

        assertTrue(api.conversations().any { it.peer?.id == "acc-bob" })
        assertTrue(api.contacts().any { it.id == "acc-bob" })
        assertTrue(api.blocked().isEmpty())
    }

    @Test
    fun `session rejects a malformed code`() = runTest {
        val api = api()
        val failure = runCatching { api.createSession("alice@example.com", "12") }.exceptionOrNull()
        assertTrue(failure is MockException)
    }

    @Test
    fun `session rejects the wrong code`() = runTest {
        val api = api()
        val failure = runCatching { api.createSession("alice@example.com", "111111") }.exceptionOrNull()
        assertNotNull(failure)
        assertTrue(failure!!.message!!.contains("wrong or expired"))
    }

    @Test
    fun `session rejects an invalid email`() = runTest {
        val api = api()
        assertTrue(runCatching { api.requestCode("alice") }.isFailure)
    }

    @Test
    fun `sign up carries the name into the account`() = runTest {
        val api = api()
        val info = api.createSession("bob@example.com", "123456", name = "bob")

        assertTrue(info.newAccount)
        assertEquals("bob", info.account.name)
        assertEquals("bob@example.com", info.account.email)
        assertEquals("bob", api.me().name)
    }

    @Test
    fun `creating a group puts it on top of the chat list`() = runTest {
        val api = api()
        val group = api.createGroup("release train", listOf("acc-bob", "acc-carol"))

        assertTrue(group.isGroup)
        assertEquals("release train", group.title)
        assertEquals(group.id, api.conversations().first().id)
        assertEquals("3 members", group.group?.subtitle)
    }

    @Test
    fun `profile rename sticks`() = runTest {
        val api = api()
        val renamed = api.updateProfile(name = "alice liddell")

        assertEquals("alice liddell", renamed.name)
        assertEquals("alice liddell", api.me().name)
    }

    @Test
    fun `forced server error surfaces from every call`() = runTest {
        val api = api(MockConfig(latencyMs = 0, forceError = ForceError.Unauthorized))
        assertTrue(runCatching { api.me() }.isFailure)
        assertTrue(runCatching { api.conversations() }.isFailure)
        assertTrue(runCatching { api.blocked() }.isFailure)
    }

    @Test
    fun `empty scenario empties the lists but keeps the account`() = runTest {
        val api = api(MockConfig(latencyMs = 0, emptyData = true))

        assertTrue(api.conversations().isEmpty())
        assertTrue(api.contacts().isEmpty())
        assertTrue(api.agents().isEmpty())
        assertEquals("alice", api.me().name)
    }

    @Test
    fun `report records the reason`() = runTest {
        val api = api()
        api.report("acc-bob", "spam")
        // 没有返回值可断言，这里只确认调用不抛错且不误改业务数据
        assertEquals(4, api.conversations().size)
    }

    // ---------------------------------------------------------------- group management

    @Test
    fun `group detail starts with four members and alice as owner`() = runTest {
        val detail = api().groupDetail("grp-release")

        assertEquals("v2.3 release", detail.name)
        assertEquals(4, detail.members.size)
        assertEquals("acc-alice", detail.ownerId)
        assertEquals("acc-alice", detail.myId)
    }

    @Test
    fun `renaming a group changes the detail and the chat list`() = runTest {
        val api = api()
        api.renameGroup("grp-release", "v2.4 release")

        assertEquals("v2.4 release", api.groupDetail("grp-release").name)
        assertEquals("v2.4 release", api.conversations().first { it.id == 2L }.title)
    }

    @Test
    fun `removing a member drops the row and the member count`() = runTest {
        val api = api()
        api.removeFromGroup("grp-release", "acc-dave")

        val detail = api.groupDetail("grp-release")
        assertEquals(3, detail.members.size)
        assertFalse(detail.members.any { it.id == "acc-dave" })
        assertEquals("3 members", api.conversations().first { it.id == 2L }.group?.subtitle)
    }

    @Test
    fun `inviting a contact adds them back`() = runTest {
        val api = api()
        api.removeFromGroup("grp-release", "acc-dave")
        api.inviteToGroup("grp-release", "acc-dave")

        assertTrue(api.groupDetail("grp-release").members.any { it.id == "acc-dave" })
    }

    @Test
    fun `the owner can't leave without transferring or dissolving`() = runTest {
        val api = api()

        val failure = runCatching { api.leaveGroup("grp-release") }.exceptionOrNull()

        assertTrue(failure is MockException)
        // 退群被拒，群还在
        assertTrue(api.conversations().any { it.id == 2L })
    }

    @Test
    fun `a member can leave and loses the conversation`() = runTest {
        val api = api()
        api.setGroupOwner("grp-release", "acc-bob")

        api.leaveGroup("grp-release")

        assertFalse(api.conversations().any { it.id == 2L })
        assertFalse(api.contacts().any { it.id == "grp-release" })
    }

    @Test
    fun `dissolving removes the group from the chat list`() = runTest {
        val api = api()
        api.dissolveGroup("grp-release")

        assertFalse(api.conversations().any { it.id == 2L })
    }

    @Test
    fun `avatar upload keeps the local uri until it is cleared`() = runTest {
        val api = api()
        assertNull(api.me().avatarUrl)

        assertEquals("content://media/picked/1", api.uploadAvatar("content://media/picked/1").avatarUrl)
        assertEquals("content://media/picked/1", api.me().avatarUrl)

        assertNull(api.clearAvatar().avatarUrl)
        assertNull(api.me().avatarUrl)
    }

    @Test
    fun `opening a direct chat clears only that unread`() = runTest {
        val api = api()
        assertEquals(5, api.conversations().sumOf { it.pending })

        api.markConversationRead(1L, upTo = 999L)

        assertEquals(0, api.conversations().first { it.id == 1L }.pending)
        // 其它对话的未读不受影响：5 - 2 = 3
        assertEquals(3, api.conversations().sumOf { it.pending })
    }

    @Test
    fun `opening a group clears its unread`() = runTest {
        val api = api()
        assertEquals(3, api.conversations().first { it.id == 2L }.pending)

        api.markConversationRead(2L, upTo = 999L)

        assertEquals(0, api.conversations().first { it.id == 2L }.pending)
    }

    @Test
    fun `changing the email needs a six digit code sent to the new address`() = runTest {
        val api = api()
        val before = api.me().email

        // 没先要验证码，直接换要被拒：这和服务端「只认发到新邮箱的码」一致
        var refused = false
        try {
            api.changeEmail("alice.new@example.com", "123456")
        } catch (_: MockException) {
            refused = true
        }
        assertTrue(refused)

        api.requestEmailCode("alice.new@example.com")
        val updated = api.changeEmail("alice.new@example.com", "654321")

        assertEquals("alice.new@example.com", updated.email)
        assertEquals("alice.new@example.com", api.me().email)
        assertFalse(before == updated.email)
    }

    @Test
    fun `same email is refused before a code goes out`() = runTest {
        val api = api()
        var refused = false
        try {
            api.requestEmailCode(api.me().email)
        } catch (_: MockException) {
            refused = true
        }
        assertTrue(refused)
    }

    @Test
    fun `group avatar lands on the group detail and can be cleared`() = runTest {
        val api = api()
        val groupId = api.conversations().first { it.isGroup }.group!!.id

        assertEquals("file:///pic.png", api.setGroupAvatar(groupId, "file:///pic.png"))
        assertEquals("file:///pic.png", api.groupDetail(groupId).avatarUrl)

        api.clearGroupAvatar(groupId)
        assertNull(api.groupDetail(groupId).avatarUrl)
    }

    @Test
    fun `only the owner can change the group avatar`() = runTest {
        val api = api()
        val groupId = api.conversations().first { it.isGroup }.group!!.id
        api.transferGroupOwner(groupId, "acc-bob")

        var refused = false
        try {
            api.setGroupAvatar(groupId, "file:///pic.png")
        } catch (_: MockException) {
            refused = true
        }
        assertTrue(refused)
    }
@Test
    fun `group sessions remember what the person picked`() = runTest {
        val api = api()
        val groupId = api.conversations().first { it.isGroup }.group!!.id

        api.setGroupSessions(groupId, listOf("agent-checkout"))
        assertEquals(listOf("agent-checkout"), api.groupDetail(groupId).mySessionIds)

        // 不是自己的会话不能被选中
        var refused = false
        try {
            api.setGroupSessions(groupId, listOf("someone-else"))
        } catch (_: MockException) {
            refused = true
        }
        assertTrue(refused)
    }
@Test
    fun `contact session sharing defaults to all and can be narrowed`() = runTest {
        val api = api()
        val bobId = api.contacts().first().id
        assertEquals("all", api.contactShare(bobId).share)

        val chosen = api.setContactShare(bobId, "chosen", sessions = listOf("agent-checkout"))
        assertEquals("chosen", chosen.share)
        assertEquals(listOf("agent-checkout"), chosen.sessions.map { it.id })
        assertEquals("chosen", api.contactShare(bobId).share)
    }
}