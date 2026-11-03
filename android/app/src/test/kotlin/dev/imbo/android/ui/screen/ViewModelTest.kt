package dev.imbo.android.ui.screen

import dev.imbo.android.data.Delivery
import dev.imbo.android.data.ForceError
import dev.imbo.android.data.MockConfig
import dev.imbo.android.data.UiState
import dev.imbo.android.data.mock.MockImboApi
import dev.imbo.android.data.noLatency
import dev.imbo.android.data.MainDispatcherRule
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * 状态机测试：发送必须是「先上屏 sending，再落 sent / failed」，
 * 认证必须「成功后带名字进欢迎页，失败留在原地」。
 */
class ViewModelTest {

    @get:Rule
    val mainDispatcher = MainDispatcherRule(StandardTestDispatcher())

    private fun api(config: MockConfig = MockConfig(latencyMs = 0, fakeReply = false)) =
        MockImboApi(MutableStateFlow(config))

    // ---------------------------------------------------------------- chat

    @Test
    fun `send echoes immediately then settles as sent`() = runTest {
        val api = api()
        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())
        vm.load(1L)
        advanceUntilIdle()
        val before = (vm.state.value as UiState.Content).data.size

        vm.send("hello")

        // 还没等网络回来时，气泡已经在本地位上，状态是 sending
        val optimistic = (vm.state.value as UiState.Content).data
        assertEquals(before + 1, optimistic.size)
        assertEquals("hello", optimistic.last().body)
        assertEquals(Delivery.Sending, optimistic.last().delivery)

        advanceUntilIdle()

        val settled = (vm.state.value as UiState.Content).data
        assertEquals(before + 1, settled.size)
        assertEquals(Delivery.Sent, settled.last().delivery)
    }

    @Test
    fun `failed send keeps the row and marks it failed`() = runTest {
        val api = api()
        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())
        vm.load(1L)
        advanceUntilIdle()

        // 发送前把通道切成错误
        api.config.value = api.config.value.copy(forceError = ForceError.ServerError)
        vm.send("will fail")
        advanceUntilIdle()

        val rows = (vm.state.value as UiState.Content).data
        assertEquals("will fail", rows.last().body)
        assertEquals(Delivery.Failed, rows.last().delivery)
    }

    @Test
    fun `blank message is not sent`() = runTest {
        val api = api()
        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())
        vm.load(1L)
        advanceUntilIdle()
        val before = (vm.state.value as UiState.Content).data.size

        vm.send("   ")
        advanceUntilIdle()

        assertEquals(before, (vm.state.value as UiState.Content).data.size)
    }

    @Test
    fun `picked attachment uploads and settles as sent`() = runTest {
        val api = api()
        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())
        vm.load(1L)
        advanceUntilIdle()
        val before = (vm.state.value as UiState.Content).data.size

        vm.sendAttachment("content://media/photo.jpg", "photo.jpg", "image/jpeg", 12_345)

        // 上传还没回来时，气泡已带本地附件上屏
        val optimistic = (vm.state.value as UiState.Content).data
        assertEquals(before + 1, optimistic.size)
        assertEquals(Delivery.Sending, optimistic.last().delivery)
        assertEquals("content://media/photo.jpg", optimistic.last().attachments.single().localUri)

        advanceUntilIdle()

        val settled = (vm.state.value as UiState.Content).data.last()
        assertEquals(Delivery.Sent, settled.delivery)
        assertEquals("photo.jpg", settled.attachments.single().name)
        assertEquals("content://media/photo.jpg", settled.attachments.single().localUri)
        assertEquals(12_345L, settled.attachments.single().sizeBytes)
    }

    @Test
    fun `loading older pages dedupes messages and stops when there is no more`() = runTest {
        val api = api()
        // 先灌 60 条，让第一页只拿到最近 50 条，留下更早的一页
        repeat(60) { index -> api.sendMessage(1L, "seed $index", "seed-$index") }

        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())
        vm.load(1L)
        advanceUntilIdle()

        assertEquals(50, (vm.state.value as UiState.Content).data.size)
        assertTrue(vm.hasMore.value)

        vm.loadOlder()
        advanceUntilIdle()

        val rows = (vm.state.value as UiState.Content).data
        // 3 条原始消息 + 60 条 seed，去重后不应多出重复气泡
        assertEquals(63, rows.size)
        assertEquals(rows.size, rows.map { it.id }.distinct().size)
        assertTrue(!vm.hasMore.value)

        // 没有更多之后，即使通道坏了也不该再发请求（不会冒出新提示）
        api.config.value = api.config.value.copy(forceError = ForceError.ServerError)
        vm.loadOlder()
        advanceUntilIdle()

        assertEquals(63, (vm.state.value as UiState.Content).data.size)
        assertNull(vm.notice.value)
    }

    @Test
    fun `draft is restored per conversation`() = runTest {
        val api = api()
        val drafts = dev.imbo.android.data.InMemoryDraftStorage()
        val vm = ChatViewModel(api, drafts)
        vm.load(1L)
        advanceUntilIdle()
        vm.saveDraft("half typed")

        // 重新创建 ViewModel 模拟离开聊天页再进来，草稿还在
        val restored = ChatViewModel(api, drafts)
        restored.load(1L)
        advanceUntilIdle()
        assertEquals("half typed", restored.draftFor(1L))

        // 切到另一个对话不会串稿
        restored.load(2L)
        advanceUntilIdle()
        assertEquals("", restored.draftFor(2L))
        assertEquals("half typed", restored.draftFor(1L))
    }

    @Test
    fun `quote flattens and truncates the original text`() {
        val long = "word ".repeat(40)
        val quoted = quoteDraft(long, "", "(attachment)")

        assertTrue(quoted.startsWith("> "))
        assertTrue(quoted.endsWith("…"))
        assertTrue(quoted.length <= 82)
        assertTrue(!quoted.contains("\n"))

        // 输入框已有内容时另起一行追加，空正文用占位文案
        assertEquals("typed\n> hello", quoteDraft("hello", "typed", "(attachment)"))
        assertEquals("> (attachment)", quoteDraft("  \n ", "", "(attachment)"))
    }

    @Test
    fun `opening an agent conversation marks it read`() = runTest {
        val api = api()
        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())

        vm.load(5L)
        advanceUntilIdle()

        assertEquals(0, api.agents().first { it.id == "agent-checkout" }.pending)
        assertEquals("checkout", vm.conversation.value?.title)
    }

    @Test
    fun `load reports an error state when the channel fails`() = runTest {
        val api = api(MockConfig(latencyMs = 0, forceError = ForceError.Timeout))
        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())

        vm.load(1L)
        advanceUntilIdle()

        assertTrue(vm.state.value is UiState.Error)
    }

    // ---------------------------------------------------------------- auth

    @Test
    fun `requesting a code moves to code sent`() = runTest {
        val vm = AuthViewModel(api())
        vm.requestCode("alice@example.com")
        advanceUntilIdle()

        val state = vm.state.value
        assertTrue(state is AuthUiState.CodeSent)
        assertEquals("alice@example.com", (state as AuthUiState.CodeSent).email)
    }

    @Test
    fun `requesting a code with a bad email fails in place`() = runTest {
        val vm = AuthViewModel(api())
        vm.requestCode("alice")
        advanceUntilIdle()

        assertTrue(vm.state.value is AuthUiState.Failed)
    }

    @Test
    fun `wrong code keeps the user on the code screen`() = runTest {
        val vm = AuthViewModel(api())
        vm.verify("alice@example.com", "111111")
        advanceUntilIdle()

        assertTrue(vm.state.value is AuthUiState.Failed)
    }

    @Test
    fun `good code produces the account`() = runTest {
        val vm = AuthViewModel(api())
        vm.requestCode("alice@example.com")
        advanceUntilIdle()
        vm.verify("alice@example.com", "123456")
        advanceUntilIdle()

        val state = vm.state.value
        assertTrue(state is AuthUiState.Done)
        assertEquals("alice", (state as AuthUiState.Done).account.name)
    }

    @Test
    fun `sign up keeps the chosen name`() = runTest {
        val vm = AuthViewModel(api())
        vm.requestCode("zoe@example.com", name = "zoe")
        advanceUntilIdle()
        vm.verify("zoe@example.com", "123456")
        advanceUntilIdle()

        val state = vm.state.value as AuthUiState.Done
        assertEquals("zoe", state.account.name)
        assertTrue(state.newAccount)
    }

    // ---------------------------------------------------------------- add contact

    @Test
    fun `add contact reports sent`() = runTest {
        val vm = AddContactViewModel(api())
        vm.submit("12345678")
        advanceUntilIdle()

        assertTrue(vm.state.value is AddContactState.Sent)
    }

    @Test
    fun `add contact surfaces a failure`() = runTest {
        val vm = AddContactViewModel(api(MockConfig(latencyMs = 0, forceError = ForceError.ServerError)))
        vm.submit("12345678")
        advanceUntilIdle()

        assertTrue(vm.state.value is AddContactState.Failed)
    }

    // ---------------------------------------------------------------- new group

    @Test
    fun `creating a group returns the conversation to open`() = runTest {
        val api = api()
        val vm = NewGroupViewModel(api)
        advanceUntilIdle()

        vm.toggle("acc-bob")
        vm.create("release train")
        advanceUntilIdle()

        val state = vm.state.value
        assertTrue(state is NewGroupState.Created)
        val created = state as NewGroupState.Created
        assertEquals("release train", api.conversation(created.conversationId)?.title)
    }

    // ---------------------------------------------------------------- group info

    @Test
    fun `removing a member shows pending then refreshes the list`() = runTest {
        val api = api()
        val vm = GroupInfoViewModel(api)
        vm.load(2L)
        advanceUntilIdle()

        val before = (vm.state.value.detail as UiState.Content).data
        assertEquals(4, before.members.size)

        vm.remove(before.members.first { it.id == "acc-dave" })
        // 请求还没跑完时先停在 Working，界面据此禁用入口
        assertTrue(vm.state.value.action is GroupActionState.Working)

        advanceUntilIdle()

        val after = (vm.state.value.detail as UiState.Content).data
        assertEquals(3, after.members.size)
        assertTrue(after.members.none { it.id == "acc-dave" })
    }

    @Test
    fun `leaving as a member closes the group screen`() = runTest {
        val api = api()
        api.setGroupOwner("grp-release", "acc-bob")
        val vm = GroupInfoViewModel(api)
        vm.load(2L)
        advanceUntilIdle()

        vm.leave()
        advanceUntilIdle()

        assertTrue(vm.state.value.action is GroupActionState.Closed)
        assertTrue(api.conversations().none { it.id == 2L })
    }

    @Test
    fun `the owner gets a failure instead of leaving`() = runTest {
        val api = api()
        val vm = GroupInfoViewModel(api)
        vm.load(2L)
        advanceUntilIdle()

        vm.leave()
        advanceUntilIdle()

        assertTrue(vm.state.value.action is GroupActionState.Failed)
        // 群没被删掉
        assertTrue(api.conversations().any { it.id == 2L })
    }

    // ---------------------------------------------------------------- blocked

    @Test
    fun `unblock refreshes the list`() = runTest {
        val api = api()
        api.block("acc-bob")
        val vm = BlockedViewModel(api)
        advanceUntilIdle()
        assertEquals(1, (vm.state.value as UiState.Content).data.size)

        vm.unblock("acc-bob")
        advanceUntilIdle()

        assertTrue((vm.state.value as UiState.Content).data.isEmpty())
    }

    // ---------------------------------------------------------------- account avatar

    @Test
    fun `avatar upload marks pending then stores the new picture`() = runTest {
        val api = api()
        val vm = AccountViewModel(api, api, MutableStateFlow(api.config.value))
        advanceUntilIdle()

        vm.uploadAvatar("content://media/picked/1")
        assertEquals(AvatarState.Uploading, vm.avatar.value)

        advanceUntilIdle()

        assertEquals(AvatarState.Idle, vm.avatar.value)
        assertEquals("content://media/picked/1", (vm.account.value as UiState.Content).data.avatarUrl)
    }

    @Test
    fun `avatar upload failure surfaces a failed state and keeps the old picture`() = runTest {
        val api = api()
        val vm = AccountViewModel(api, api, MutableStateFlow(api.config.value))
        advanceUntilIdle()
        api.config.value = api.config.value.copy(forceError = ForceError.ServerError)

        vm.uploadAvatar("content://media/picked/2")
        advanceUntilIdle()

        assertEquals(AvatarState.Failed, vm.avatar.value)
        assertNull((vm.account.value as UiState.Content).data.avatarUrl)
    }

    @Test
    fun `opening a direct conversation marks it read`() = runTest {
        val api = api()
        assertEquals(2, api.conversations().first { it.id == 1L }.pending)

        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())
        vm.load(1L)
        advanceUntilIdle()

        assertEquals(0, api.conversations().first { it.id == 1L }.pending)
    }

    @Test
    fun `opening a group conversation marks it read`() = runTest {
        val api = api()
        assertEquals(3, api.conversations().first { it.id == 2L }.pending)

        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())
        vm.load(2L)
        advanceUntilIdle()

        assertEquals(0, api.conversations().first { it.id == 2L }.pending)
    }

    // ---------------------------------------------------------------- sessions rename

    @Test
    fun `renaming a session updates the list title`() = runTest {
        val api = api()
        val vm = SessionsViewModel(api)
        advanceUntilIdle()

        vm.rename("agent-checkout", "orders bot")
        advanceUntilIdle()

        val sessions = (vm.state.value as UiState.Content).data
        assertEquals("orders bot", sessions.first { it.id == "agent-checkout" }.title)
        assertNull(vm.notice.value)
    }

    @Test
    fun `blank session name is rejected and keeps the old title`() = runTest {
        val api = api()
        val vm = SessionsViewModel(api)
        advanceUntilIdle()

        vm.rename("agent-checkout", "   ")
        advanceUntilIdle()

        val sessions = (vm.state.value as UiState.Content).data
        assertEquals("checkout", sessions.first { it.id == "agent-checkout" }.title)
        assertTrue(vm.notice.value?.isNotEmpty() == true)
    }

    // ---------------------------------------------------------------- group transfer

    @Test
    fun `owner hands the group over and the new owner sticks`() = runTest {
        val api = api()
        val vm = GroupInfoViewModel(api)
        vm.load(2L)
        advanceUntilIdle()

        val detail = (vm.state.value.detail as UiState.Content).data
        assertEquals(detail.myId, detail.ownerId)
        val target = detail.members.first { it.id != detail.myId }

        vm.transfer(target)
        advanceUntilIdle()

        val after = (vm.state.value.detail as UiState.Content).data
        assertEquals(target.id, after.ownerId)
        assertEquals(GroupActionState.Idle, vm.state.value.action)
    }

    @Test
    fun `transferring again after handing over is rejected`() = runTest {
        val api = api()
        val vm = GroupInfoViewModel(api)
        vm.load(2L)
        advanceUntilIdle()

        val detail = (vm.state.value.detail as UiState.Content).data
        val target = detail.members.first { it.id != detail.myId }
        vm.transfer(target)
        advanceUntilIdle()

        val other = detail.members.first { it.id != detail.myId && it.id != target.id }
        vm.transfer(other)
        advanceUntilIdle()

        assertTrue(vm.state.value.action is GroupActionState.Failed)
        val after = (vm.state.value.detail as UiState.Content).data
        assertEquals(target.id, after.ownerId)
    }
@Test
    fun `forwarding sends the same body to the target conversation`() = runTest {
        val api = api()
        val vm = ChatViewModel(api, dev.imbo.android.data.InMemoryDraftStorage())
        vm.load(2L)
        advanceUntilIdle()

        val source = (vm.state.value as UiState.Content).data.first()
        vm.forward(source, 3L)
        advanceUntilIdle()

        val delivered = api.messages(3L).messages.last()
        assertEquals(source.body, delivered.body)
        assertTrue(delivered.mine)
    }
@Test
    fun `health check surfaces the server version`() = runTest {
        val api = api()
        val vm = AccountViewModel(api, api, MutableStateFlow(api.config.value))

        vm.checkHealth()
        advanceUntilIdle()

        val state = vm.health.value
        assertTrue(state is UiState.Content)
        assertEquals("mock", (state as UiState.Content).data)
    }

    @Test
    fun `health check failure carries the reason`() = runTest {
        val api = api()
        val vm = AccountViewModel(api, api, MutableStateFlow(api.config.value))
        // 让 mock 下一次请求失败：config 是 MockImboApi 自己持有的那个 flow
        api.config.value = api.config.value.copy(forceError = ForceError.ServerError)

        vm.checkHealth()
        advanceUntilIdle()

        assertTrue(vm.health.value is UiState.Error)
    }
}