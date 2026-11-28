package com.posplatform.app.network

import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

/** Minimal JSON API client for pos-server; keeps the Android shell dependency-free. */
internal class ApiClient(
    private val baseUrl: String,
    private val token: String,
    private val openConnection: (String) -> HttpURLConnection = { url ->
        URL(url).openConnection() as HttpURLConnection
    },
) {
    companion object {
        private const val CONNECT_TIMEOUT_MS = 10_000
        private const val READ_TIMEOUT_MS = 15_000
    }

    fun post(path: String, body: JSONObject): JSONObject = request("POST", path, body.toString())

    fun get(path: String): JSONObject = request("GET", path, null)

    private fun request(method: String, path: String, body: String?): JSONObject {
        val connection = openConnection(baseUrl.trimEnd('/') + path)
        connection.requestMethod = method
        connection.connectTimeout = CONNECT_TIMEOUT_MS
        connection.readTimeout = READ_TIMEOUT_MS
        // Never replay a long-lived bearer token to a redirect target.
        connection.instanceFollowRedirects = false
        connection.setRequestProperty("Authorization", "Bearer $token")
        connection.setRequestProperty("Accept", "application/json")
        val code: Int
        val text: String
        try {
            if (body != null) {
                connection.setRequestProperty("Content-Type", "application/json")
                connection.doOutput = true
                connection.outputStream.use { it.write(body.toByteArray(Charsets.UTF_8)) }
            }
            code = connection.responseCode
            val stream = if (code in 200..299) connection.inputStream else connection.errorStream
            text = stream?.bufferedReader()?.use { it.readText() }.orEmpty()
        } finally {
            connection.disconnect()
        }

        if (code !in 200..299) {
            throw IOException("HTTP $code: ${errorMessage(text)}")
        }
        return JSONObject(text.ifEmpty { "{}" })
    }

    private fun errorMessage(text: String): String = runCatching {
        JSONObject(text).optJSONObject("error")?.optString("message")?.takeIf { it.isNotEmpty() }
    }.getOrNull() ?: text
}
