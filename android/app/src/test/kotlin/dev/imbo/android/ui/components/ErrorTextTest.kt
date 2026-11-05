package dev.imbo.android.ui.components

import dev.imbo.android.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * 服务端错误 → 本地化文案的映射。这条逻辑直接决定用户看到什么，
 * 而且全是字符串匹配，用单测钉住比每次真机造错误快得多。
 */
class ErrorTextTest {

    @Test
    fun `contact related errors map to their own strings`() {
        assertEquals(R.string.err_already_contact, serverErrorRes("carol is already your contact."))
        assertEquals(R.string.err_not_contact, serverErrorRes("79830691 is not your contact. Add them first."))
    }

    @Test
    fun `auth errors map to login wording`() {
        assertEquals(R.string.err_signed_out, serverErrorRes("You are signed out on this phone. Sign in again."))
        assertEquals(R.string.auth_err_code, serverErrorRes("That code is wrong or expired."))
    }

    @Test
    fun `rate limit and ownership errors are recognised`() {
        assertEquals(R.string.err_rate_limited, serverErrorRes("A code was just sent. Wait a minute before asking again."))
        assertEquals(R.string.err_rate_limited, serverErrorRes("Too many requests"))
        assertEquals(
            R.string.err_not_owner,
            serverErrorRes("Only the owner of ship room (#7119466) can make someone else the owner."),
        )
        assertEquals(
            R.string.err_group_not_invited,
            serverErrorRes("42 is not in release crew. Invite them first."),
        )
    }

    @Test
    fun `network failures map to one friendly line`() {
        assertEquals(R.string.err_network, serverErrorRes("failed to connect to /10.0.0.5 (port 8080) after 15000ms"))
        assertEquals(R.string.err_network, serverErrorRes("unexpected end of stream on http://127.0.0.1:8080/…"))
        assertEquals(R.string.err_network, serverErrorRes("java.net.ConnectException: Connection refused"))
        assertEquals(R.string.err_network, serverErrorRes("unable to resolve host \"imbo.example.com\""))
        assertEquals(R.string.err_network_timeout, serverErrorRes("SocketTimeoutException: timeout"))
    }

    @Test
    fun `unknown or empty messages stay unmapped`() {
        assertNull(serverErrorRes(null))
        assertNull(serverErrorRes(""))
        assertNull(serverErrorRes("something nobody predicted"))
    }
}
