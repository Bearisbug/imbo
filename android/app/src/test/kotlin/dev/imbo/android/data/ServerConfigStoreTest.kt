package dev.imbo.android.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * 服务器地址是用户手输的，格式整理错了就会连到别处；
 * 这里把「补协议、去斜杠、拒非法」这些规则钉住。
 */
class ServerConfigStoreTest {

    @Test
    fun `a bare host gets https`() {
        assertEquals("https://imbo.example.com", ServerConfigStore.normalize("imbo.example.com"))
        assertEquals("https://my-pc.tail1234.ts.net", ServerConfigStore.normalize("my-pc.tail1234.ts.net"))
    }

    @Test
    fun `scheme and port are kept and trailing slashes dropped`() {
        assertEquals("http://10.0.0.5:8080", ServerConfigStore.normalize("http://10.0.0.5:8080/"))
        assertEquals("https://host:8443", ServerConfigStore.normalize("https://host:8443///"))
    }

    @Test
    fun `a path prefix survives for reverse proxies`() {
        assertEquals("https://example.com/imbo", ServerConfigStore.normalize("example.com/imbo/"))
    }

    @Test
    fun `blank and malformed values are refused`() {
        assertNull(ServerConfigStore.normalize(""))
        assertNull(ServerConfigStore.normalize("   "))
        assertNull(ServerConfigStore.normalize("ftp://example.com"))
        assertNull(ServerConfigStore.normalize("https://"))
        assertNull(ServerConfigStore.normalize("https://exa mple.com"))
        assertNull(ServerConfigStore.normalize("https://example.com?x=1"))
        assertNull(ServerConfigStore.normalize("https://example.com#frag"))
    }

    @Test
    fun `displayHost hides the scheme`() {
        assertEquals("imbo.example.com", ServerConfigStore.displayHost("https://imbo.example.com/"))
        assertEquals("10.100.83.217:8080", ServerConfigStore.displayHost("http://10.100.83.217:8080"))
    }
}
