// Remote control (PLAN.md §9 "Remote adapters", M6): the client's end of
// its control channel to the server. A port of
// crates/marathon-client/src/remote.rs over OkHttp's WebSocket.
//
// RemoteSource keeps a WebSocket open to the server's /clients/connect and
// hands every Action the server relays (from POST /clients/{id}/actions) to
// the action bus. In the other direction, publish sends the client's
// Events up the same socket, for the server to republish on GET /events.
//
//   curl, home automation, ... ─► server ─WebSocket─► RemoteSource ─► action bus
//              observers ◄─ GET /events ◄─ server ◄─WebSocket─ publish ◄─ tuner
//
// If it cannot connect, or the connection drops (the server restarted, the
// network went away), it reconnects with exponential Backoff. OkHttp's
// ping interval notices a network that silently went away. Events
// published while disconnected are not queued; instead, on every
// (re)connect it re-sends the events that describe the client's current
// state (Replay), so the server's GET /clients is right again at once.
package marathon.core

import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import kotlinx.serialization.SerializationException
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

/** The name the source's actions carry on the bus. */
const val REMOTE_SOURCE_NAME: String = "remote"

/** How often to ping the server; a missed pong means the connection is dead. */
private const val PING_EVERY_S = 15L

/** How long to wait for the server to accept a connection. */
private const val CONNECT_TIMEOUT_S = 10L

/**
 * Exponential backoff between connection attempts: [initialMs], doubling up
 * to [maxMs], and back to [initialMs] once a connection succeeds. No
 * jitter: a household has a handful of clients.
 */
class Backoff(val initialMs: Long = 500, val maxMs: Long = 30_000) {
    private var last: Long? = null

    /** How long to wait before the next attempt. */
    fun nextDelayMs(): Long {
        val delay = last?.let { minOf(it * 2, maxMs) } ?: initialMs
        last = delay
        return delay
    }

    /** Starts over from [initialMs], after a successful connection. */
    fun reset() {
        last = null
    }
}

/** The client's current state, as the events that describe it. */
class Replay {
    private var playing: Event? = null
    private var paused: Event? = null
    private var standby = false

    fun observe(event: Event) {
        when (event) {
            is Event.NowPlaying -> {
                playing = event
                paused = null
                standby = false
            }
            is Event.Paused -> paused = event
            is Event.Resumed -> paused = null
            Event.Standby -> standby = true
            Event.Wake -> standby = false
            is Event.ChannelChanged, is Event.ItemEnded, is Event.Error -> {}
        }
    }

    /** What to send after connecting, in order. */
    fun events(): List<Event> = listOfNotNull(playing, paused) + if (standby) listOf(Event.Standby) else emptyList()
}

/**
 * The remote-control input. See the file comment. Nothing connects until
 * [start]; [stop] closes the socket and stops reconnecting.
 *
 * @param url the control channel's `ws://` or `wss://` URL ([HttpApi.controlUrl]).
 * @param http a client that adds the client-ID and token headers ([HttpApi.http]).
 * @param onAction receives relayed actions, on OkHttp's thread; it should only forward.
 */
class RemoteSource(
    private val url: String,
    http: OkHttpClient,
    private val onAction: (Action) -> Unit,
    private val backoff: Backoff = Backoff(),
) {
    private val http = http.newBuilder()
        .pingInterval(PING_EVERY_S, TimeUnit.SECONDS)
        .connectTimeout(CONNECT_TIMEOUT_S, TimeUnit.SECONDS)
        // The socket is long-lived; the API's per-call timeout must not apply.
        .callTimeout(0, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.SECONDS)
        .build()
    private val scheduler: ScheduledExecutorService = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "remote").apply { isDaemon = true }
    }
    private val lock = Any()
    private val replay = Replay()
    private var socket: WebSocket? = null
    private var connected = false
    private var stopped = false
    private var failures = 0

    fun start() {
        synchronized(lock) { if (!stopped) connect() }
    }

    /** Closes the connection and stops reconnecting. */
    fun stop() {
        synchronized(lock) {
            stopped = true
            socket?.close(1000, "client stopping")
            socket = null
            connected = false
        }
        scheduler.shutdownNow()
    }

    /** Sends [event] up the control channel if connected (see the file comment). Never blocks. */
    fun publish(event: Event) {
        synchronized(lock) {
            replay.observe(event)
            if (connected) socket?.send(event.toJson())
        }
    }

    /** Whether the control channel is up. */
    fun isConnected(): Boolean = synchronized(lock) { connected }

    // Called with `lock` held.
    private fun connect() {
        val request = Request.Builder().url(url).build()
        socket = http.newWebSocket(request, Listener())
    }

    private inner class Listener : WebSocketListener() {
        private fun current(webSocket: WebSocket) = socket === webSocket

        override fun onOpen(webSocket: WebSocket, response: Response) {
            synchronized(lock) {
                if (!current(webSocket) || stopped) return
                Log.info("remote control connected to $url")
                connected = true
                failures = 0
                backoff.reset()
                for (event in replay.events()) webSocket.send(event.toJson())
            }
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            val action = try {
                ControlJson.decodeFromString(Action.serializer(), text)
            } catch (e: SerializationException) {
                Log.warn("ignoring a relayed message that is not an action: ${e.message}")
                return
            } catch (e: IllegalArgumentException) {
                Log.warn("ignoring a relayed message that is not an action: ${e.message}")
                return
            }
            Log.debug("relayed action $action")
            onAction(action)
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(1000, null)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = lost(webSocket, "closed ($code $reason)")

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            val why = response?.let { "the server refused the connection (${it.code})" } ?: (t.message ?: t.toString())
            lost(webSocket, why)
        }

        private fun lost(webSocket: WebSocket, why: String) {
            synchronized(lock) {
                if (!current(webSocket) || stopped) return
                val wasConnected = connected
                connected = false
                socket = null
                if (wasConnected) {
                    Log.warn("remote control disconnected ($why); reconnecting")
                } else {
                    failures += 1
                    // Say so once; a server that stays down should not fill the log.
                    if (failures == 1) {
                        Log.warn("cannot connect to $url for remote control ($why); will keep trying")
                    } else {
                        Log.debug("remote control still cannot connect ($why), $failures tries")
                    }
                }
                val delay = backoff.nextDelayMs()
                scheduler.schedule({ synchronized(lock) { if (!stopped && socket == null) connect() } }, delay, TimeUnit.MILLISECONDS)
            }
        }
    }
}
