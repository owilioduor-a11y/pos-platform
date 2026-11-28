package com.posplatform.app.network

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.net.HttpURLConnection
import java.net.URL
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ApiClientTest {

    private class FakeConnection(url: URL) : HttpURLConnection(url) {
        var disconnected: Boolean = false

        override fun connect() = Unit

        override fun disconnect() {
            disconnected = true
        }

        override fun usingProxy(): Boolean = false

        override fun getResponseCode(): Int = 302

        override fun getInputStream() = ByteArrayInputStream("{}".toByteArray())

        override fun getErrorStream() = ByteArrayInputStream("{}".toByteArray())

        override fun getOutputStream() = ByteArrayOutputStream()
    }

    @Test
    fun sets_timeouts_and_blocks_redirects_with_bearer_token() {
        val connection = FakeConnection(URL("http://example.test/start"))
        val client = ApiClient("http://example.test", "secret-token") { connection }

        assertTrue(runCatching { client.get("/start") }.isFailure)

        assertFalse(connection.instanceFollowRedirects)
        assertEquals(10_000, connection.connectTimeout)
        assertEquals(15_000, connection.readTimeout)
        assertEquals("Bearer secret-token", connection.getRequestProperty("Authorization"))
        assertTrue(connection.disconnected)
    }
}
