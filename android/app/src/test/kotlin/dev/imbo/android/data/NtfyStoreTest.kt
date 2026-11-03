package dev.imbo.android.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ntfy 话题由设备标识派生：同样的输入必须得到同样的话题，否则重装后
 * 用户得重新订阅一次，推送就"断"了。这条规则用单测钉住。
 */
class NtfyStoreTest {

    @Test
    fun `same device and package always give the same topic`() {
        val first = NtfyStore.topicFrom("android-id-123", "dev.imbo.android")
        val second = NtfyStore.topicFrom("android-id-123", "dev.imbo.android")
        assertEquals(first, second)
    }

    @Test
    fun `topic looks like a safe ntfy name`() {
        val topic = NtfyStore.topicFrom("android-id-123", "dev.imbo.android")
        assertTrue(topic.startsWith("imbo-"))
        // ntfy 话题只允许 [A-Za-z0-9_-]，服务端校验也是这条规则
        assertTrue(topic.drop(5).all { it.isLetterOrDigit() || it == '-' || it == '_' })
        assertEquals(17, topic.length)
    }

    @Test
    fun `different devices and packages get different topics`() {
        val base = NtfyStore.topicFrom("android-id-123", "dev.imbo.android")
        assertNotEquals(base, NtfyStore.topicFrom("android-id-456", "dev.imbo.android"))
        assertNotEquals(base, NtfyStore.topicFrom("android-id-123", "dev.imbo.other"))
    }

    @Test
    fun `missing device id still yields a usable topic`() {
        val topic = NtfyStore.topicFrom("", "dev.imbo.android")
        assertTrue(topic.startsWith("imbo-"))
        assertEquals(17, topic.length)
    }
}
