// RemoteSource against a real WebSocket (OkHttp's MockWebServer), plus
// Backoff and Replay. Mirrors the tests in
// crates/marathon-client/src/remote.rs.
@file:Suppress("FunctionName")

package marathon.core

import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

class RemoteTest {
    @Test
    fun backoff_doubles_to_the_cap_and_resets() {
        val b = Backoff(initialMs = 500, maxMs = 30_000)
        assertEquals(listOf(500L, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000), List(8) { b.nextDelayMs() })
        b.reset()
        assertEquals(500L, b.nextDelayMs())
    }

    @Test
    fun replay_describes_the_current_state() {
        val r = Replay()
        assertEquals(emptyList<Event>(), r.events())
        val playing = Event.NowPlaying("c1", 1, "i1", "Pilot", 0)
        val paused = Event.Paused("c1", "i1", 5_000)
        r.observe(Event.ChannelChanged("c1", 1, "One"))
        r.observe(playing)
        r.observe(paused)
        r.observe(Event.Standby)
        assertEquals(listOf(playing, paused, Event.Standby), r.events())
        r.observe(Event.Wake)
        r.observe(Event.Resumed("c1", "i1", 5_000))
        assertEquals(listOf<Event>(playing), r.events())
        // A new item clears pause and standby.
        r.observe(paused)
        r.observe(Event.Standby)
        val next = Event.NowPlaying("c1", 1, "i2", "Two", 0)
        r.observe(next)
        assertEquals(listOf<Event>(next), r.events())
    }

    /** The server's end of one control-channel connection. */
    private class ServerSide : WebSocketListener() {
        val opened = LinkedBlockingQueue<WebSocket>()
        val received = LinkedBlockingQueue<String>()

        override fun onOpen(webSocket: WebSocket, response: Response) {
            opened.put(webSocket)
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            received.put(text)
        }

        // Answer the client's close, as the real server does.
        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(1000, null)
        }
    }

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

    @Test
    fun relays_actions_publishes_events_and_reconnects_with_a_replay() {
        val first = ServerSide()
        val second = ServerSide()
        server.enqueue(MockResponse.Builder().webSocketUpgrade(first).build())
        server.enqueue(MockResponse.Builder().webSocketUpgrade(second).build())

        val api = HttpApi(server.url("/").toString(), "secret", "client-1")
        val actions = LinkedBlockingQueue<Action>()
        val remote = RemoteSource(api.controlUrl(), api.http, { actions.put(it) }, Backoff(initialMs = 10, maxMs = 50))
        try {
            remote.start()
            val socket = first.opened.poll(5, TimeUnit.SECONDS)!!
            val handshake = server.takeRequest()
            assertEquals("/api/v1/clients/connect", handshake.url.encodedPath)
            assertEquals("client-1", handshake.headers[CLIENT_ID_HEADER])
            assertEquals("Bearer secret", handshake.headers["Authorization"])

            // Server → client: a relayed action reaches the bus.
            socket.send("""{"action":"go_to_channel","number":4}""")
            assertEquals(Action.GoToChannel(4), actions.poll(5, TimeUnit.SECONDS))
            // Garbage is ignored, not fatal.
            socket.send("""{"action":"self_destruct"}""")
            socket.send("""{"action":"channel_up"}""")
            assertEquals(Action.ChannelUp, actions.poll(5, TimeUnit.SECONDS))

            // Client → server: events go up the socket.
            awaitConnected(remote)
            val playing = Event.NowPlaying("c1", 1, "i1", "Pilot", 0)
            remote.publish(playing)
            assertEquals(playing.toJson(), first.received.poll(5, TimeUnit.SECONDS))
            remote.publish(Event.Paused("c1", "i1", 1_000))
            first.received.poll(5, TimeUnit.SECONDS)

            // The server goes away; the client reconnects and replays its state.
            socket.close(1001, "server restarting")
            second.opened.poll(5, TimeUnit.SECONDS)!!
            assertEquals(playing.toJson(), second.received.poll(5, TimeUnit.SECONDS))
            assertEquals(Event.Paused("c1", "i1", 1_000).toJson(), second.received.poll(5, TimeUnit.SECONDS))
        } finally {
            remote.stop()
        }
    }

    private fun awaitConnected(remote: RemoteSource) {
        val until = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (!remote.isConnected() && System.nanoTime() < until) Thread.sleep(10)
        assertTrue(remote.isConnected())
    }
}
