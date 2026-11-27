package com.posplatform.app.network

import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

/** Minimal JSON API client for pos-server; keeps the Android shell dependency-free. */
internal class ApiClient(
    private val baseUrl: String,
    private val token: String,
) {
    fun post(path: String, body: JSONObject): JSONObject = request("POST", path, body.toString())

    fun get(path: String): JSONObject = request("GET", path, null)

    private fun request(method: String, path: String, body: String?): JSONObject {
        val connection = URL(baseUrl.trimEnd('/') + path).openConnection() as HttpURLConnection
        connection.requestMethod = method
        connection.setRequestProperty("Authorization", "Bearer $token")
        connection.setRequestProperty("Accept", "application/json")
        if (body != null) {
            connection.setRequestProperty("Content-Type", "application/json")
            connection.doOutput = true
            connection.outputStream.use { it.write(body.toByteArray(Charsets.UTF_8)) }
        }

        val code = connection.responseCode
        val stream = if (code in 200..299) connection.inputStream else connection.errorStream
        val text = stream?.bufferedReader()?.use { it.readText() }.orEmpty()
        connection.disconnect()

        if (code !in 200..299) {
            throw IOException("HTTP $code: ${errorMessage(text)}")
        }
        return JSONObject(text.ifEmpty { "{}" })
    }

    private fun errorMessage(text: String): String = runCatching {
        JSONObject(text).optJSONObject("error")?.optString("message")?.takeIf { it.isNotEmpty() }
    }.getOrNull() ?: text
}
