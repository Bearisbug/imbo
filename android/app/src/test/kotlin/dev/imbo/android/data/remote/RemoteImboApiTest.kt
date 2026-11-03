package dev.imbo.android.data.remote

import dev.imbo.android.data.InMemorySessionState
import dev.imbo.android.data.Access
import kotlinx.coroutines.test.runTest
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * 真实客户端的契约测试：用 MockWebServer 喂服务端的真实响应形状，
 * 断言请求路径、请求体和映射结果。没有服务端也能验证这块。
 */
class RemoteImboApiTest {

    private lateinit var server: MockWebServer
    private lateinit var session: InMemorySessionState

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        session = InMemorySessionState()
    }

    @After
    fun tearDown() {
        server.shutdown()
    }

    private fun api(
        files: LocalFileSource = LocalFileSource { "file-bytes".toByteArray() },
        pictures: PictureSource = PictureSource {
            UploadPicture(byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte()), "image/jpeg")
        },
    ): RemoteImboApi {
        val http = ImboHttp(server.url("/").toString(), session)
        // 附件缓存用临时目录，避免依赖 Android Context
        val store = dev.imbo.android.data.AttachmentStore(
            java.nio.file.Files.createTempDirectory("imbo-att").toFile(),
        )
        return RemoteImboApi(http, session, EventSocket(http, session), files, pictures, store)
    }

    private fun json(body: String, code: Int = 200): MockResponse =
        MockResponse()
            .setResponseCode(code)
            .setHeader("Content-Type", "application/json")
            .setBody(body)

    private val loginBody = """
        {"account":{"id":81234567,"name":"alice","email":"alice@example.com"},
         "token":"tok-123","new_account":false}
    """.trimIndent()

    private val meBody = """
        {"account":{"id":81234567,"name":"alice","email":"alice@example.com"},
         "access":{"state":"trial","until":"2030-01-01T00:00:00Z"}}
    """.trimIndent()

    @Test
    fun `login posts the android platform and keeps the token`() = runTest {
        server.enqueue(json(loginBody))
        server.enqueue(json(meBody))

        val result = api().createSession("alice@example.com", "123456", null)

        val login = server.takeRequest()
        assertEquals("POST", login.method)
        assertEquals("/v1/app/session", login.path)
        val sent = login.body.readUtf8()
        assertTrue(sent.contains("\"platform\":\"android\""))
        assertTrue(sent.contains("\"code\":\"123456\""))
        assertFalse(sent.contains("\"name\""))

        assertEquals("tok-123", session.token)
        assertEquals(81234567, session.accountId)
        assertEquals("81234567", result.account.accountId)
        assertFalse(result.newAccount)
        assertTrue(result.account.access is Access.Trial)
    }

    @Test
    fun `authorized calls carry the bearer token`() = runTest {
        session.token = "tok-abc"
        server.enqueue(json(meBody))

        api().me()

        assertEquals("Bearer tok-abc", server.takeRequest().getHeader("Authorization"))
    }

    @Test
    fun `conversations map peer, preview and unread`() = runTest {
        session.accountId = 1
        server.enqueue(
            json(
                """
                {"conversations":[
                  {"id":7,"kind":"direct",
                   "peer":{"id":42,"name":"bob"},
                   "last_message":{"id":99,"from":{"account_id":42,"name":"bob"},
                                   "preview":"Migration is live","created_at":"2026-01-01T10:00:00Z"},
                   "pending":2},
                  {"id":8,"kind":"group","group":{"id":5,"name":"v2.3 release"},
                   "pending":3}
                ]}
                """.trimIndent(),
            ),
        )

        val list = api().conversations()

        assertEquals(2, list.size)
        val direct = list.first()
        assertEquals("bob", direct.peer?.name)
        assertEquals(2, direct.pending)
        assertEquals("Migration is live", direct.lastMessage?.preview)
        assertFalse(direct.lastMessage!!.mine)
        assertTrue(list.last().isGroup)
        assertEquals("v2.3 release", list.last().title)
    }

    @Test
    fun `sending a direct message addresses the peer account id`() = runTest {
        session.accountId = 1
        server.enqueue(
            json("""{"conversations":[{"id":7,"kind":"direct","peer":{"id":42,"name":"bob"},"pending":0}]}"""),
        )
        server.enqueue(json("""{"message_id":555,"conversation":{"kind":"direct","id":7}}"""))

        val api = api()
        api.conversations()
        val message = api.sendMessage(7L, "hello", "client-1")

        server.takeRequest() // conversations
        val send = server.takeRequest()
        assertEquals("/v1/messages", send.path)
        val sent = send.body.readUtf8()
        assertTrue(sent.contains("\"to\":\"42\""))
        assertTrue(sent.contains("\"client_msg_id\":\"client-1\""))
        assertEquals(555L, message.id)
        assertTrue(message.mine)
    }

    @Test
    fun `uploading posts the raw bytes with name and hash headers`() = runTest {
        session.token = "tok-abc"
        server.enqueue(
            json(
                """{"file":{"id":"f1","name":"photo.jpg","size":1234,
                     "sha256":"00","expires_at":"2030-01-01T00:00:00Z"},
                     "quota":{"used":1234,"limit":1000000,"resets_at":"2030-01-01T00:00:00Z"}}""".trimIndent(),
            ),
        )

        val attachment = api()
            .uploadAttachment("file:///tmp/photo.jpg", "photo.jpg", "image/jpeg")

        val req = server.takeRequest()
        assertEquals("POST", req.method)
        assertEquals("/v1/files", req.path)
        // 服务端读原始请求体，不是 multipart
        assertEquals("image/jpeg", req.getHeader("Content-Type"))
        assertEquals("photo.jpg", req.getHeader("Imbo-File-Name"))
        val expectedSha = java.security.MessageDigest.getInstance("SHA-256")
            .digest("file-bytes".toByteArray())
            .joinToString("") { "%02x".format(it) }
        assertEquals(expectedSha, req.getHeader("Imbo-File-Sha256"))
        assertEquals(64, req.getHeader("Imbo-File-Sha256")!!.length)
        assertEquals("file-bytes", req.body.readUtf8())
        assertEquals("f1", attachment.id)
        // 上传成功后会把字节缓存到本机，localUri 指向缓存文件（按文件 id 命名）
        assertTrue(attachment.localUri!!.startsWith("file://"))
        assertTrue(attachment.localUri!!.endsWith("f1.jpg"))
        assertEquals(1234L, attachment.sizeBytes)
    }

    @Test
    fun `file names are percent encoded for the upload header`() {
        assertEquals("photo.jpg", ImboHttp.percentEncode("photo.jpg"))
        // 中文和空格都要编码，空格是 %20 而不是 +
        assertEquals("%E6%88%91%E7%9A%84%20a.pdf", ImboHttp.percentEncode("我的 a.pdf"))
    }

    @Test
    fun `sending with attachments puts file ids into the body`() = runTest {
        session.accountId = 1
        server.enqueue(
            json("""{"conversations":[{"id":7,"kind":"direct","peer":{"id":42,"name":"bob"},"pending":0}]}"""),
        )
        server.enqueue(json("""{"message_id":558,"conversation":{"kind":"direct","id":7}}"""))

        val api = api()
        api.conversations()
        val attachment = dev.imbo.android.data.Attachment(
            id = "f1",
            name = "photo.jpg",
            mimeType = "image/jpeg",
            localUri = "file:///tmp/photo.jpg",
            sizeBytes = 10,
        )
        api.sendMessage(7L, "", "client-3", listOf(attachment))

        server.takeRequest() // conversations
        val sent = server.takeRequest().body.readUtf8()
        assertTrue(sent.contains("\"files\":[\"f1\"]"))
    }

    @Test
    fun `sending to a group uses the hash address`() = runTest {
        session.accountId = 1
        server.enqueue(
            json("""{"conversations":[{"id":8,"kind":"group","group":{"id":5,"name":"release"},"pending":0}]}"""),
        )
        server.enqueue(json("""{"message_id":556,"conversation":{"kind":"group","id":8}}"""))

        val api = api()
        api.conversations()
        api.sendMessage(8L, "ship it", "client-2")

        server.takeRequest()
        assertTrue(server.takeRequest().body.readUtf8().contains("\"to\":\"#5\""))
    }

    @Test
    fun `agent sessions become negative conversation ids`() = runTest {
        session.accountId = 1
        server.enqueue(
            json(
                """
                {"agents":[{"id":"3f1a8f0e-0000-0000-0000-000000000000","short_id":"7",
                            "title":"checkout","tool":"Claude Code","online":true,"unread":1}]}
                """.trimIndent(),
            ),
        )

        val agents = api().agents()

        assertEquals(1, agents.size)
        assertEquals("checkout", agents.first().title)
        assertEquals("online", agents.first().state)
        assertEquals(1, agents.first().pending)
        assertTrue(Mapper.agentConversationId(agents.first().id) < 0)
    }

    @Test
    fun `message files map to attachments`() = runTest {
        session.accountId = 1
        server.enqueue(
            json(
                """
                {"messages":[{"id":9,"from":{"account_id":42,"name":"bob"},
                  "conversation":{"kind":"direct","id":7},"body":"spec",
                  "created_at":"2026-01-01T10:00:00Z",
                  "files":[{"id":"f9","name":"spec-v2.pdf","size":48200,
                            "sha256":"00","expires_at":"2030-01-01T00:00:00Z"}]}],
                 "has_more":false}
                """.trimIndent(),
            ),
        )

        val attachment = api().messages(7L).messages.single().attachments.single()

        assertEquals("spec-v2.pdf", attachment.name)
        assertEquals(48200L, attachment.sizeBytes)
        assertEquals("application/pdf", attachment.mimeType)
        assertNull(attachment.localUri)
    }

    @Test
    fun `agent messages load through the agents endpoint`() = runTest {
        session.accountId = 1
        val uuid = "3f1a8f0e-0000-0000-0000-000000000000"
        val synthetic = Mapper.agentConversationId(uuid)
        // conversation(id) 会先取 agents
        server.enqueue(
            json("""{"agents":[{"id":"$uuid","short_id":"7","title":"checkout","tool":"Claude Code","online":true,"unread":0}]}"""),
        )
        server.enqueue(json("""{"messages":[],"has_more":false}"""))

        api().messages(synthetic)

        server.takeRequest() // agents
        assertEquals("/v1/agents/$uuid/messages?limit=50", server.takeRequest().path)
    }

    @Test
    fun `a problem response surfaces status and detail`() = runTest {
        server.enqueue(
            json(
                """{"type":"https://imbo.dev/errors/code-invalid","title":"Code invalid",
                    "status":400,"detail":"That code is wrong or expired."}""",
                code = 400,
            ),
        )

        val failure = runCatching { api().createSession("alice@example.com", "111111", null) }.exceptionOrNull()

        assertTrue(failure is ImboApiException)
        val e = failure as ImboApiException
        assertEquals(400, e.status)
        assertEquals("That code is wrong or expired.", e.message)
    }

    @Test
    fun `contacts that I blocked are filtered out`() = runTest {
        server.enqueue(
            json(
                """
                {"contacts":[
                  {"id":42,"name":"bob","sessions":[{"short_id":"1","title":"orders-api","tool":"Claude Code","online":true}]},
                  {"id":43,"name":"dave","blocked":true}
                ]}
                """.trimIndent(),
            ),
        )

        val contacts = api().contacts()

        assertEquals(1, contacts.size)
        assertEquals("bob", contacts.first().name)
        assertTrue(contacts.first().online)
    }

    @Test
    fun `reporting maps the reason to the server vocabulary`() = runTest {
        server.enqueue(json("""{"report":{"id":1,"created_at":"2026-01-01T00:00:00Z"}}"""))

        api().report("42", "Harassment")

        val req = server.takeRequest()
        assertEquals("/v1/reports", req.path)
        val sent = req.body.readUtf8()
        assertTrue(sent.contains("\"account_id\":42"))
        assertTrue(sent.contains("\"reason\":\"harassment\""))
    }

    @Test
    fun `blocking sends the numeric account id`() = runTest {
        server.enqueue(json("""{"block":{"account":{"id":42,"name":"bob"}}}"""))

        api().block("42")

        val req = server.takeRequest()
        assertEquals("/v1/blocks", req.path)
        assertTrue(req.body.readUtf8().contains("\"account_id\":42"))
    }

    @Test
    fun `signing out clears the stored token`() = runTest {
        session.token = "tok-abc"
        session.accountId = 81234567
        server.enqueue(MockResponse().setResponseCode(204))

        api().signOut()

        assertEquals("DELETE", server.takeRequest().method)
        assertNull(session.token)
        assertEquals(0, session.accountId)
    }

    @Test
    fun `email change is rejected with a clear reason`() = runTest {
        val failure = runCatching { api().updateProfile(email = "new@example.com") }.exceptionOrNull()

        assertTrue(failure is ImboApiException)
        assertEquals(400, (failure as ImboApiException).status)
    }

    @Test
    fun `missing conversation messages become an empty page`() = runTest {
        server.enqueue(
            json(
                """{"type":"https://imbo.dev/errors/not-found","title":"Not found","status":404,"detail":"No such conversation."}""",
                code = 404,
            ),
        )

        val page = api().messages(1234L)

        assertTrue(page.messages.isEmpty())
        assertFalse(page.hasMore)
    }

    @Test
    fun `avatar upload sends the raw picture bytes as an image`() = runTest {
        session.token = "tok-abc"
        server.enqueue(json("""{"avatar_url":"/v1/avatars/abc.jpg"}"""))
        server.enqueue(
            json(
                """{"account":{"id":81234567,"name":"alice","avatar_url":"https://cdn.imbo.dev/v1/avatars/abc.jpg"},
                     "access":{"state":"trial","until":"2030-01-01T00:00:00Z"}}""".trimIndent(),
            ),
        )
        val picture = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 1, 2)
        val api = api(
            pictures = PictureSource {
                assertEquals("content://media/picked/1", it)
                UploadPicture(picture, "image/jpeg")
            },
        )

        val account = api.uploadAvatar("content://media/picked/1")

        val put = server.takeRequest()
        assertEquals("PUT", put.method)
        assertEquals("/v1/me/avatar", put.path)
        assertEquals("image/jpeg", put.getHeader("Content-Type"))
        assertArrayEquals(picture, put.body.readByteArray())
        assertEquals("https://cdn.imbo.dev/v1/avatars/abc.jpg", account.avatarUrl)
    }

    @Test
    fun `relative avatar urls are resolved against the api base`() = runTest {
        server.enqueue(
            json("""{"account":{"id":1,"name":"alice","avatar_url":"/v1/avatars/abc.jpg"}}"""),
        )

        val account = api().me()

        assertEquals(server.url("/v1/avatars/abc.jpg").toString(), account.avatarUrl)
    }

    @Test
    fun `clearing the avatar deletes it and returns the account without one`() = runTest {
        server.enqueue(MockResponse().setResponseCode(204))
        server.enqueue(json(meBody))

        val account = api().clearAvatar()

        assertEquals("DELETE", server.takeRequest().method)
        assertNull(account.avatarUrl)
    }

    @Test
    fun `renaming an agent puts the trimmed title`() = runTest {
        session.token = "tok-abc"
        val uuid = "3f1a8f0e-0000-0000-0000-000000000000"
        server.enqueue(
            json(
                """{"agent":{"id":"$uuid","short_id":"7","title":"orders bot","tool":"Claude Code","online":true,"unread":0}}""",
            ),
        )

        api().renameAgent(uuid, "  orders bot  ")

        val put = server.takeRequest()
        assertEquals("PUT", put.method)
        assertEquals("/v1/agents/$uuid/title", put.path)
        assertEquals("""{"title":"orders bot"}""", put.body.readUtf8())
    }

    @Test
    fun `handing over the group sends the new owner account id`() = runTest {
        session.token = "tok-abc"
        server.enqueue(json("""{"group":{"id":5,"name":"release","owner":{"id":42,"name":"bob"}}}"""))

        api().transferGroupOwner("5", "42")

        val put = server.takeRequest()
        assertEquals("PUT", put.method)
        assertEquals("/v1/groups/5/owner", put.path)
        assertEquals("""{"account_id":42}""", put.body.readUtf8())
    }

    @Test
    fun `email change asks for a code and posts it with the new address`() = runTest {
        session.token = "tok-abc"
        server.enqueue(json("""{"retry_after_s":60}""", code = 202))
        server.enqueue(json("""{"account":{"id":81234567,"name":"alice","email":"new@example.com"}}"""))
        server.enqueue(json(meBody))

        val api = api()
        api.requestEmailCode("new@example.com")
        val code = server.takeRequest()
        assertEquals("POST", code.method)
        assertEquals("/v1/me/email/code", code.path)
        assertEquals("""{"email":"new@example.com"}""", code.body.readUtf8())

        api.changeEmail("new@example.com", "654321")
        val change = server.takeRequest()
        assertEquals("POST", change.method)
        assertEquals("/v1/me/email", change.path)
        assertEquals("""{"email":"new@example.com","code":"654321"}""", change.body.readUtf8())
    }

    @Test
    fun `deleting the account sends the confirm body and drops the token`() = runTest {
        session.token = "tok-abc"
        server.enqueue(MockResponse().setResponseCode(204))

        api().deleteAccount()

        val delete = server.takeRequest()
        assertEquals("DELETE", delete.method)
        assertEquals("/v1/account", delete.path)
        // 服务端要求正文明确写 DELETE，缺了会回 400
        assertEquals("""{"confirm":"DELETE"}""", delete.body.readUtf8())
        assertNull(session.token)
    }

    @Test
    fun `group avatar uploads raw bytes and returns an absolute url`() = runTest {
        session.token = "tok-abc"
        server.enqueue(json("""{"avatar_url":"/v1/avatars/abc.jpg"}"""))

        val url = api().setGroupAvatar("7119466", "content://media/picked/1")

        val put = server.takeRequest()
        assertEquals("PUT", put.method)
        assertEquals("/v1/groups/7119466/avatar", put.path)
        assertEquals("image/jpeg", put.getHeader("Content-Type"))
        assertEquals(3, put.bodySize)
        assertTrue(url!!.startsWith(server.url("/").toString()))
    }
}
