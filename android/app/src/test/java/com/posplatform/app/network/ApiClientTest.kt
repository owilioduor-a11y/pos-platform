package com.posplatform.app.network

import com.sun.net.httpserver.HttpServer
import java.net.InetSocketAddress
import java.util.Collections
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ApiClientTest {

    @Test
    fun does_not_follow_redirect_with_bearer_token() {
        val seen = Collections.synchronizedList(mutableListOf<String>())
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        server.createContext("/start") { exchange ->
            seen.add(exchange.requestHeaders.getFirst("Authorization").orEmpty())
            exchange.responseHeaders.set("Location", "/target")
            exchange.sendResponseHeaders(302, -1)
            exchange.close()
        }
        server.createContext("/target") { exchange ->
            seen.add(exchange.requestHeaders.getFirst("Authorization").orEmpty())
            exchange.sendResponseHeaders(200, -1)
            exchange.close()
        }
        server.start()
        try {
            val client = ApiClient("http://127.0.0.1:${server.address.port}", "secret-token")
            assertTrue(runCatching { client.get("/start") }.isFailure)
            assertEquals(listOf("Bearer secret-token"), seen)
        } finally {
            server.stop(0)
        }
    }
}
