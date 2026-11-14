// One connection to a server, while the Watch screen is up: the Android
// equivalent of the desktop client's main(), wiring the pieces from :core
// together (docs/ANDROID.md):
//
//   keys, touch ─┐
//   remote (WS) ─┼─► TunerLoop (tuner thread) ─► Tuner ─► MpvPlayer
//   mpv events ──┘                                  │
//                                     Events ───────┼─► RemoteSource (GET /events)
//                                     UiEvents ─────┴─► OverlayModel ─► Compose
//
// A Session belongs to SessionHolder, not to an activity, so that audio-only
// playback can outlive the Watch screen (PlaybackService).
package marathon.android

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import marathon.core.Action
import marathon.core.Event
import marathon.core.HttpApi
import marathon.core.Log
import marathon.core.RemoteSource
import marathon.core.SystemClock
import marathon.core.Tuner
import marathon.core.TunerConfig
import marathon.core.TunerLoop
import marathon.core.overlay.OverlayModel

/** What's on, for the media notification and lock screen. */
data class NowPlaying(val channelNumber: Int, val channelName: String, val title: String)

class Session(context: Context, server: ServerSettings, clientId: String, onQuit: () -> Unit) {
    val api = HttpApi(server.url, server.token, clientId)
    val overlay = OverlayModel()

    private val _changes = MutableStateFlow(0L)

    /** Bumped whenever the overlay changes, so the UI redraws it. */
    val changes: StateFlow<Long> = _changes

    private fun changed() {
        _changes.value += 1
    }

    private lateinit var loop: TunerLoop<MpvPlayer>

    private val nowPlayingState = MutableStateFlow<NowPlaying?>(null)

    /** The channel and item playing, as the Tuner reports them (null before the first). */
    val nowPlaying: StateFlow<NowPlaying?> = nowPlayingState

    /** The channel's name from its last ChannelChanged, which NowPlaying lacks. */
    @Volatile private var channelName = ""

    val player = MpvPlayer(context, api.mediaHeaders) { loop.playerEvent(it) }

    private val remote = RemoteSource(api.controlUrl(), api.http, { send(it, "remote") })

    init {
        val tuner = Tuner(
            api = api,
            player = player,
            clock = SystemClock,
            config = TunerConfig(clientId = clientId),
            events = { event ->
                remote.publish(event)
                when (event) {
                    is Event.Error -> toast(event.message)
                    is Event.ChannelChanged -> channelName = event.channelName
                    is Event.NowPlaying ->
                        nowPlayingState.value = NowPlaying(event.channelNumber, channelName, event.title)
                    else -> {}
                }
            },
            ui = { cue ->
                overlay.cue(cue, SystemClock.nowMs())
                changed()
            },
        )
        loop = TunerLoop(tuner, onError = ::toast, onQuit = onQuit)
    }

    fun start() {
        Log.info("connecting to ${api.base}")
        remote.start()
        loop.launch()
    }

    /** Sends [action] from the input called [source]. Any thread. */
    fun send(action: Action, source: String) {
        loop.send(action, source)
    }

    /** Reports progress and stops everything. Blocks briefly. */
    fun close() {
        loop.shutdown()
        remote.stop()
        player.destroy()
    }

    /** Shows [message] in the overlay's toast. Any thread. */
    fun toast(message: String) {
        overlay.toast(message, SystemClock.nowMs())
        changed()
    }
}
