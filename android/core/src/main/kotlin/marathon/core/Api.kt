// Talking to a Marathon server's v1 API (PLAN.md §8). A port of
// crates/marathon-client/src/api.rs.
//
// [Api] is the interface the Tuner uses, so it can be tested against an
// in-memory fake. [HttpApi] implements it over the client generated from
// the server's OpenAPI document (the :api module). Calls are blocking; the
// app makes them from the tuner thread, never the main thread.
package marathon.core

import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlinx.serialization.SerializationException
import marathon.api.apis.ChannelsApi
import marathon.api.apis.HistoryApi
import marathon.api.apis.ServerApi
import marathon.api.infrastructure.ClientError
import marathon.api.infrastructure.ClientException
import marathon.api.infrastructure.Serializer
import marathon.api.infrastructure.ServerError
import marathon.api.infrastructure.ServerException
import marathon.api.models.AdvanceRequest
import marathon.api.models.ChannelSummary
import marathon.api.models.ErrorBody
import marathon.api.models.ErrorCode
import marathon.api.models.HistoryEntry
import marathon.api.models.Item
import marathon.api.models.Now
import marathon.api.models.PreviousRequest
import marathon.api.models.ProgressReport
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient

/** The header that identifies a client to the server. */
const val CLIENT_ID_HEADER: String = "X-Marathon-Client"

/** A failed API call. */
sealed class ApiError(message: String) : Exception(message) {
    /** The server URL is not usable. */
    class BadUrl(val url: String, val reason: String) : ApiError("invalid server URL \"$url\": $reason")

    /** The request did not get a response (connection refused, timeout, ...). */
    class Transport(val reason: String) : ApiError("cannot reach the server: $reason")

    /** The server answered with an error status. */
    class Server(
        val status: Int,
        /** The machine-readable code, if the body was an [ErrorBody]. */
        val code: ErrorCode?,
        val serverMessage: String,
    ) : ApiError("server returned $status: $serverMessage")

    /** The response body was not what the API promises. */
    class Decode(val reason: String) : ApiError("unexpected response from the server: $reason")

    /** The server's error code, if the server sent one. */
    fun code(): ErrorCode? = (this as? Server)?.code
}

/**
 * The API calls a client makes. Every URL in a returned value (`streamUrl`,
 * subtitle `url`) is absolute, ready to hand to a player.
 */
interface Api {
    /** `GET /channels`: every channel, by number. */
    fun channels(): List<ChannelSummary>

    /** `GET /channels/{id}/now`: tunes in. */
    fun now(channelId: String): Now

    /** `POST /channels/{id}/progress`. */
    fun progress(channelId: String, report: ProgressReport)

    /** `POST /channels/{id}/advance`. */
    fun advance(channelId: String, request: AdvanceRequest): Now

    /** `POST /channels/{id}/previous`. */
    fun previous(channelId: String, request: PreviousRequest): Now

    /** `GET /history?limit=`: recent events, newest first. */
    fun history(limit: Int): List<HistoryEntry>
}

/** How long to wait to connect, and for a whole request. */
private const val CONNECT_TIMEOUT_S = 5L
private const val REQUEST_TIMEOUT_S = 15L

/**
 * [Api] over HTTP, for the server at [baseUrl] (e.g. `http://192.168.1.5:7878`).
 * [token] is sent as a bearer token if set; [clientId] identifies this
 * client in the server's history.
 */
class HttpApi(baseUrl: String, token: String?, clientId: String) : Api {
    /** The server's base URL, always ending in `/` so joining keeps any path prefix. */
    val base: HttpUrl

    /**
     * Headers a player must send when fetching streams and subtitles (the
     * client ID, and the token if any), as `(name, value)` pairs.
     */
    val mediaHeaders: List<Pair<String, String>>

    /** The HTTP client, with [mediaHeaders] on every request. Share it (e.g. for the WebSocket). */
    val http: OkHttpClient

    private val channelsApi: ChannelsApi
    private val historyApi: HistoryApi
    private val serverApi: ServerApi

    init {
        val parsed = baseUrl.trim().toHttpUrlOrNull()
            ?: throw ApiError.BadUrl(baseUrl, "not an http or https URL")
        base = if (parsed.encodedPath.endsWith("/")) {
            parsed
        } else {
            parsed.newBuilder().encodedPath(parsed.encodedPath + "/").build()
        }
        mediaHeaders = buildList {
            add(CLIENT_ID_HEADER to clientId)
            if (!token.isNullOrEmpty()) add("Authorization" to "Bearer $token")
        }
        http = OkHttpClient.Builder()
            .connectTimeout(CONNECT_TIMEOUT_S, TimeUnit.SECONDS)
            .callTimeout(REQUEST_TIMEOUT_S, TimeUnit.SECONDS)
            .addInterceptor { chain ->
                val request = chain.request().newBuilder()
                for ((name, value) in mediaHeaders) request.header(name, value)
                chain.proceed(request.build())
            }
            .build()
        // The generated client appends `/api/v1/...` to its base path.
        val basePath = base.toString().removeSuffix("/")
        channelsApi = ChannelsApi(basePath, http)
        historyApi = HistoryApi(basePath, http)
        serverApi = ServerApi(basePath, http)
    }

    /**
     * Resolves a server-relative URL (as found in API responses, e.g.
     * `/api/v1/stream/x`) against the base URL, keeping any path prefix.
     */
    fun resolve(url: String): String = base.resolve(url.trimStart('/'))?.toString() ?: url

    /** The URL of this client's control channel (`/clients/connect`), as `ws://` or `wss://`. */
    fun controlUrl(): String {
        val http = base.resolve("api/v1/clients/connect").toString()
        return when {
            http.startsWith("https:") -> "wss:" + http.removePrefix("https:")
            else -> "ws:" + http.removePrefix("http:")
        }
    }

    /** `GET /health`: whether the server is up and the token is accepted. */
    fun health(): String = call { serverApi.health().status }

    override fun channels(): List<ChannelSummary> = call {
        channelsApi.listChannels().map { c -> c.copy(current = c.current?.let(::absoluteItem)) }
    }

    override fun now(channelId: String): Now = call { absoluteNow(channelsApi.now(channelId)) }

    override fun progress(channelId: String, report: ProgressReport) = call { channelsApi.progress(channelId, report) }

    override fun advance(channelId: String, request: AdvanceRequest): Now =
        call { absoluteNow(channelsApi.advance(channelId, request)) }

    override fun previous(channelId: String, request: PreviousRequest): Now =
        call { absoluteNow(channelsApi.previous(channelId, request)) }

    override fun history(limit: Int): List<HistoryEntry> = call { historyApi.history(limit) }

    private fun absoluteItem(item: Item): Item = item.copy(subtitles = item.subtitles.map { it.copy(url = resolve(it.url)) })

    private fun absoluteNow(now: Now): Now = now.copy(
        streamUrl = resolve(now.streamUrl),
        audioUrl = now.audioUrl?.let(::resolve),
        item = absoluteItem(now.item),
        next = now.next?.let {
            it.copy(streamUrl = resolve(it.streamUrl), audioUrl = it.audioUrl?.let(::resolve), item = absoluteItem(it.item))
        },
    )

    /** Runs a generated-client call, mapping its exceptions onto [ApiError]. */
    private fun <T> call(block: () -> T): T = try {
        block()
    } catch (e: ClientException) {
        throw serverError(e.statusCode, (e.response as? ClientError<*>)?.body)
    } catch (e: ServerException) {
        throw serverError(e.statusCode, (e.response as? ServerError<*>)?.body)
    } catch (e: IOException) {
        throw ApiError.Transport(e.message ?: e.toString())
    } catch (e: SerializationException) {
        throw ApiError.Decode(e.message ?: e.toString())
    } catch (e: IllegalArgumentException) {
        // kotlinx.serialization reports some malformed input this way.
        throw ApiError.Decode(e.message ?: e.toString())
    }

    private fun serverError(status: Int, body: Any?): ApiError {
        val text = body?.toString().orEmpty()
        val parsed = try {
            Serializer.kotlinxSerializationJson.decodeFromString(ErrorBody.serializer(), text)
        } catch (_: SerializationException) {
            null
        } catch (_: IllegalArgumentException) {
            null
        }
        return when (parsed) {
            null -> ApiError.Server(status, null, text.ifEmpty { "HTTP $status" })
            else -> ApiError.Server(status, parsed.code, parsed.message)
        }
    }
}
