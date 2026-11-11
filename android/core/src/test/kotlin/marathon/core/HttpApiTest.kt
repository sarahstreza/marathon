// HttpApi against a mock HTTP server: routes, headers, URL resolution, and
// error mapping. Mirrors the tests in crates/marathon-client/src/api.rs.
@file:Suppress("FunctionName")

package marathon.core

import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import marathon.api.models.ErrorCode
import marathon.api.models.ProgressReport
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

class HttpApiTest {
    private lateinit var server: MockWebServer

    @BeforeEach
    fun setUp() {
        server = MockWebServer()
        server.start()
    }

    @AfterEach
    fun tearDown() {
        server.close()
    }

    private fun base(prefix: String = "") = server.url("/$prefix").toString().removeSuffix("/")

    private fun json(body: String, code: Int = 200) =
        MockResponse.Builder().code(code).addHeader("Content-Type", "application/json").body(body).build()

    private val nowJson = """
        {"channel_id":"office","channel_name":"The Office","channel_number":1,
         "item":{"id":"i1","source_id":"lib","path":"a.mkv","title":"Pilot","missing":false,
                 "subtitles":[{"index":0,"label":"en","format":"srt","url":"/api/v1/subtitles/i1/0","language":"en"}]},
         "position_ms":1000,"stream_url":"/api/v1/stream/i1",
         "next":{"item":{"id":"i2","source_id":"lib","path":"b.mkv","title":"Two","missing":false,"subtitles":[]},
                 "stream_url":"/api/v1/stream/i2"}}
    """.trimIndent()

    @Test
    fun routes_headers_and_absolute_urls() {
        server.enqueue(json(nowJson))
        val api = HttpApi(base("marathon"), "secret", "client-1")
        val now = api.now("a b/c")
        val request = server.takeRequest()
        assertEquals("/marathon/api/v1/channels/a%20b%2Fc/now", request.url.encodedPath)
        assertEquals("client-1", request.headers[CLIENT_ID_HEADER])
        assertEquals("Bearer secret", request.headers["Authorization"])
        assertEquals(base("marathon") + "/api/v1/stream/i1", now.streamUrl)
        assertEquals(base("marathon") + "/api/v1/subtitles/i1/0", now.item.subtitles[0].url)
        assertEquals(base("marathon") + "/api/v1/stream/i2", now.next!!.streamUrl)
    }

    @Test
    fun no_token_means_no_authorization_header() {
        server.enqueue(json("[]"))
        val api = HttpApi(base(), null, "c")
        assertEquals(emptyList<Any>(), api.channels())
        assertNull(server.takeRequest().headers["Authorization"])
        assertEquals(listOf(CLIENT_ID_HEADER to "c"), api.mediaHeaders)
    }

    @Test
    fun control_url_is_a_websocket_url_under_the_base() {
        assertEquals("ws://127.0.0.1:7878/api/v1/clients/connect", HttpApi("http://127.0.0.1:7878", null, "c").controlUrl())
        assertEquals(
            "wss://example.com/marathon/api/v1/clients/connect",
            HttpApi("https://example.com/marathon/", null, "c").controlUrl(),
        )
    }

    @Test
    fun progress_posts_json() {
        server.enqueue(MockResponse.Builder().code(204).build())
        HttpApi(base(), null, "c").progress("office", ProgressReport(itemId = "i1", positionMs = 5_000, durationMs = null, paused = false))
        val request = server.takeRequest()
        assertEquals("POST", request.method)
        assertEquals("/api/v1/channels/office/progress", request.url.encodedPath)
        val body = request.body!!.utf8()
        assertEquals(true, body.contains("\"item_id\":\"i1\"") && body.contains("\"position_ms\":5000"), body)
    }

    @Test
    fun error_bodies_become_server_errors_with_their_code() {
        server.enqueue(json("""{"code":"stale_item","message":"stale"}""", 409))
        val err = assertThrows<ApiError.Server> {
            HttpApi(base(), null, "c").progress("office", ProgressReport(itemId = "x", positionMs = 0))
        }
        assertEquals(409, err.status)
        assertEquals(ErrorCode.STALE_ITEM, err.code())
        assertEquals("server returned 409: stale", err.message)

        server.enqueue(MockResponse.Builder().code(502).body("bad gateway").build())
        val plain = assertThrows<ApiError.Server> { HttpApi(base(), null, "c").channels() }
        assertNull(plain.code)
        assertEquals("server returned 502: bad gateway", plain.message)
    }

    @Test
    fun rejects_unusable_urls() {
        for (bad in listOf("not a url", "ftp://host/", "mailto:x@y")) {
            assertThrows<ApiError.BadUrl>(bad) { HttpApi(bad, null, "c") }
        }
    }

    @Test
    fun unreachable_server_is_a_transport_error() {
        // Port 1 on loopback refuses connections.
        assertThrows<ApiError.Transport> { HttpApi("http://127.0.0.1:1", null, "c").channels() }
    }
}
