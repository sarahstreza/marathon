// The Tuner's Player, backed by libmpv (dev.jdtech.mpv's build of
// mpv-android's native libraries). The Android twin of
// apps/marathon-desktop/src/player.rs; see docs/ANDROID.md "Playback".
//
// This MPVLib wrapper delivers events as bare IDs: END_FILE carries
// neither its reason nor its playlist entry, as it does on desktop. So the
// latest load's fate is read from properties instead:
//
// - Started: FILE_LOADED while mpv's `path` is the URL we last loaded.
// - Ended: keep-open=yes makes mpv pause on the last frame at the end of a
//   file instead of unloading it, so `eof-reached` turning true is the end.
// - Failed: mpv only goes idle (`idle-active`) when the file it was on
//   could not be opened or broke while playing, or when we stopped it.
//   Replacing a file never passes through idle. So idle while the latest
//   load is pending or playing is a failure.
//
// Events for files that were replaced are ignored, so Ended/Failed only
// ever refer to the latest load, as the Player contract requires. (Counting
// END_FILEs instead would break when mpv replaces a queued file before
// starting it, as rapid channel flipping does.)
//
// Audio-only mode (PLAN.md M12, docs/ANDROID.md "Audio only"): with
// setAudioOnly(true), loads play the server's audio stream for the item
// (LoadRequest.audioUrl) with video off, instead of the video stream. A
// stream starts where it is asked to, at timestamp zero, so positions are
// offset by where it started (AudioStream), its duration comes from the
// server, and seeking or switching modes reopens the stream at the
// current position. Such reopens replace the file like any load (so the
// Tuner just sees another Started), and keep the pause state.
//
// A long pause can let the stream's connection drop. Then mpv either
// goes idle (an error) or reaches the end of the file early. Rather than
// reporting a failure or an end, which would skip the rest of the item,
// the player reopens the stream once where it stopped. It does this again
// only after playing on a while, so a stream that keeps dying still
// reaches the Tuner as Failed.
//
// All mpv calls are thread-safe. load and friends run on the tuner
// thread, setAudioOnly on the main thread; `opening` serializes them.
// mpv's callbacks arrive on its event thread and only forward
// PlayerEvents to the tuner's queue, or hand a reconnect to `reopener`.
package marathon.android

import android.content.Context
import android.view.Surface
import dev.jdtech.mpv.MPVLib
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import marathon.core.AudioStream
import marathon.core.Log
import marathon.core.LoadRequest
import marathon.core.Player
import marathon.core.PlayerError
import marathon.core.PlayerEvent
import marathon.core.SubtitleTrack
import java.util.Locale
import java.util.concurrent.Executors

class MpvPlayer(
    context: Context,
    /** Sent with every HTTP request mpv makes (the client ID and token). */
    headers: List<Pair<String, String>>,
    /** Receives what mpv's events mean to the tuner, on mpv's event thread. */
    private val onEvent: (PlayerEvent) -> Unit,
) : Player, MPVLib.EventObserver {
    private val mpv: MPVLib = MPVLib.create(context.applicationContext)
        ?: throw PlayerError("cannot start mpv")

    // Cached from observed properties, so reads never wait on mpv.
    @Volatile private var timePos: Double? = null

    @Volatile private var duration: Double? = null

    private val pausedState = MutableStateFlow(false)

    /** Whether mpv is paused, as it changes (for picture-in-picture's play/pause button). */
    val paused: StateFlow<Boolean> = pausedState

    private val aspectState = MutableStateFlow<Double?>(null)

    /** The video's display aspect ratio (width / height), or null with no video. */
    val videoAspect: StateFlow<Double?> = aspectState

    private val lock = Any()

    /** The URL of the latest load, while it is pending or playing (not ended, failed, or stopped). */
    private var latestUrl: String? = null

    /** mpv has started a file since the latest load (so an idle now is about that load). */
    private var startedSinceLoad = false

    /** The latest load has opened (FILE_LOADED) and not yet ended or failed. */
    private var loaded = false

    /** Subtitles to add once the latest load has opened. */
    private var pendingSubtitles: List<SubtitleTrack> = emptyList()

    /** The Tuner's latest load, for reopening it (seeks, mode switches, reconnects). */
    private var request: LoadRequest? = null

    /** Whether the latest load is an audio stream, and where in the item that stream starts. */
    private var playingAudio = false
    private var offsetMs = 0L

    /** Item position at the latest reconnect, while the reconnect hasn't played on [RECONNECT_AGAIN_MS]. */
    private var reconnectedAtMs: Long? = null

    /** Serializes loads and reopens from different threads. Never taken by mpv's callbacks. */
    private val opening = Any()

    /** Runs reconnects off mpv's event thread. */
    private val reopener = Executors.newSingleThreadExecutor { Thread(it, "mpv-reopen").apply { isDaemon = true } }

    @Volatile private var audioOnlyWanted = false

    /** mpv's own `cache-secs`, restored for video. */
    private val defaultCacheSecs: String

    private val audioOnlyState = MutableStateFlow(false)

    /** Whether audio-only mode is on, as it changes (whether or not the current item has an audio stream). */
    val audioOnly: StateFlow<Boolean> = audioOnlyState

    init {
        for ((name, value) in OPTIONS) mpv.setOptionString(name, value)
        mpv.init()
        for ((name, value) in headers) {
            // `append` adds one entry; setting the whole list would split values on commas.
            mpv.command(arrayOf("change-list", "http-header-fields", "append", "$name: $value"))
        }
        defaultCacheSecs = mpv.getPropertyString("cache-secs") ?: "3600"
        mpv.addObserver(this)
        mpv.observeProperty("time-pos", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
        mpv.observeProperty("duration", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
        mpv.observeProperty("pause", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
        mpv.observeProperty("eof-reached", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
        mpv.observeProperty("idle-active", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
        mpv.observeProperty("video-params/aspect", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
    }

    // --- Surface (called by the video view on the main thread) ------------

    @Volatile private var destroyed = false

    fun attachSurface(surface: Surface) {
        if (destroyed) return
        mpv.attachSurface(surface)
        mpv.setOptionString("force-window", "yes")
        mpv.setPropertyString("vo", VIDEO_OUTPUT)
    }

    fun surfaceSizeChanged(width: Int, height: Int) {
        if (destroyed) return
        mpv.setPropertyString("android-surface-size", "${width}x$height")
    }

    fun detachSurface() {
        if (destroyed) return
        mpv.setPropertyString("vo", "null")
        mpv.setOptionString("force-window", "no")
        mpv.detachSurface()
    }

    /** Frees mpv. Nothing may use this player afterwards. */
    fun destroy() {
        if (destroyed) return
        destroyed = true
        reopener.shutdownNow()
        mpv.removeObserver(this)
        mpv.destroy()
    }

    // --- Audio-only mode (any thread) -------------------------------------

    /**
     * Turns audio-only mode on or off. What is playing switches at the
     * current position. Returns false, changing nothing, if [on] and the
     * current item has no audio stream (the server offers none).
     */
    fun setAudioOnly(on: Boolean): Boolean = synchronized(opening) {
        val current = synchronized(lock) { request.takeIf { latestUrl != null } }
        if (on && current != null && current.audioUrl == null) return false
        audioOnlyWanted = on
        audioOnlyState.value = on
        if (current != null && (on && current.audioUrl != null) != playingAudio) {
            val at = positionMs() ?: current.startMs
            Log.info("audio only ${if (on) "on" else "off"} at ${at}ms")
            try {
                open(current, at, unpause = false)
            } catch (e: PlayerError) {
                // mpv refused the command itself; the next load tries again.
                Log.warn("cannot switch to ${if (on) "audio" else "video"}: $e")
            }
        }
        true
    }


    // --- Player -------------------------------------------------------------

    override fun load(request: LoadRequest) {
        synchronized(opening) {
            synchronized(lock) { reconnectedAtMs = null }
            open(request, request.startMs, unpause = true)
        }
    }

    /**
     * Plays [request] from [startMs]: its audio stream in audio-only mode
     * (when it has one), else its video. [unpause] for the Tuner's loads;
     * reopens keep the pause state. Call with `opening` held.
     */
    private fun open(request: LoadRequest, startMs: Long, unpause: Boolean) {
        val audioUrl = request.audioUrl?.takeIf { audioOnlyWanted }
        val url = audioUrl?.let { AudioStream.url(it, startMs) } ?: request.url
        synchronized(lock) {
            this.request = request
            playingAudio = audioUrl != null
            offsetMs = if (audioUrl != null) startMs else 0
            latestUrl = url
            startedSinceLoad = false
            loaded = false
            // Sidecar subtitles only go with video.
            pendingSubtitles = if (audioUrl != null) emptyList() else request.subtitles
        }
        timePos = null
        duration = null
        mpv.setPropertyString("vid", if (audioUrl != null) "no" else "auto")
        // mpv reads network streams far ahead by default. For video that
        // smooths playback; for audio it would fetch (and have the server
        // transcode) up to an hour on every channel flip.
        mpv.setPropertyString("cache-secs", if (audioUrl != null) AUDIO_CACHE_SECS else defaultCacheSecs)
        // `start` applies to the next file loaded; it is set before every
        // load, so it never leaks between items. An audio stream already
        // starts there.
        val start = if (audioUrl != null) 0L else startMs
        mpv.setPropertyString("start", String.format(Locale.ROOT, "%.3f", start / 1000.0))
        if (unpause) {
            mpv.setPropertyBoolean("pause", false)
            pausedState.value = false
        }
        try {
            mpv.command(arrayOf("loadfile", url, "replace"))
        } catch (e: RuntimeException) {
            throw PlayerError(e.message ?: e.toString())
        }
    }

    override fun stop() {
        synchronized(opening) {
            synchronized(lock) {
                latestUrl = null
                loaded = false
                request = null
                playingAudio = false
            }
            mpv.command(arrayOf("stop"))
        }
    }

    override fun setPaused(paused: Boolean) {
        mpv.setPropertyBoolean("pause", paused)
        pausedState.value = paused
    }

    override fun isPaused(): Boolean = pausedState.value

    override fun seekRelative(seconds: Int) {
        synchronized(opening) {
            val current = synchronized(lock) { request.takeIf { playingAudio && latestUrl != null } }
            if (current == null) {
                mpv.command(arrayOf("seek", seconds.toString(), "relative"))
                return
            }
            // An audio stream can't seek: reopen it at the new position.
            val target = AudioStream.seekTarget(positionMs() ?: offsetMs, seconds, current.durationMs)
            try {
                open(current, target, unpause = false)
            } catch (e: PlayerError) {
                Log.warn("cannot seek the audio stream: $e")
            }
        }
    }

    override fun positionMs(): Long? {
        val (audio, offset) = synchronized(lock) { playingAudio to offsetMs }
        if (audio) return AudioStream.positionMs(offset, timePos)
        return timePos?.takeIf { it.isFinite() && it >= 0 }?.let { Math.round(it * 1000) }
    }

    override fun durationMs(): Long? {
        // An audio stream has no duration of its own; the server's stands in.
        synchronized(lock) { if (playingAudio) return request?.durationMs }
        return duration?.takeIf { it.isFinite() && it > 0 }?.let { Math.round(it * 1000) }
    }

    override fun toggleSubtitles() {
        if (synchronized(lock) { playingAudio }) return
        mpv.command(arrayOf("cycle", "sub"))
        Log.info("subtitles: track ${mpv.getPropertyString("sid")}")
    }

    // --- mpv events (mpv's event thread) ----------------------------------

    override fun event(eventId: Int) {
        when (eventId) {
            MPVLib.MpvEvent.MPV_EVENT_START_FILE -> synchronized(lock) { if (latestUrl != null) startedSinceLoad = true }
            MPVLib.MpvEvent.MPV_EVENT_FILE_LOADED -> {
                // Reading `path` here is safe: mpv's core is on this file
                // until it processes the next command, and if that was a
                // newer load, the path won't match.
                val path = mpv.getPropertyString("path")
                val subtitles = synchronized(lock) {
                    if (loaded || latestUrl == null || path != latestUrl) return
                    loaded = true
                    pendingSubtitles.also { pendingSubtitles = emptyList() }
                }
                addSubtitles(subtitles)
                onEvent(PlayerEvent.Started)
            }
        }
    }

    override fun eventProperty(property: String) {
        // The property became unavailable (e.g. nothing loaded).
        when (property) {
            "time-pos" -> timePos = null
            "duration" -> duration = null
            "video-params/aspect" -> aspectState.value = null
        }
    }

    override fun eventProperty(property: String, value: Long) {}

    override fun eventProperty(property: String, value: Double) {
        when (property) {
            "time-pos" -> timePos = value
            "duration" -> duration = value
            "video-params/aspect" -> aspectState.value = value.takeIf { it.isFinite() && it > 0 }
        }
    }

    override fun eventProperty(property: String, value: Boolean) {
        when (property) {
            "pause" -> pausedState.value = value
            "eof-reached" -> if (value) {
                if (streamDropped()) return
                val ended = synchronized(lock) {
                    loaded.also {
                        if (it) {
                            loaded = false
                            latestUrl = null
                        }
                    }
                }
                if (ended) onEvent(PlayerEvent.Ended)
            }
            "idle-active" -> if (value) {
                if (synchronized(lock) { loaded } && reconnect("the audio stream failed")) return
                val failure = synchronized(lock) {
                    when {
                        latestUrl == null || !startedSinceLoad -> null
                        loaded -> "playback failed"
                        else -> "the stream could not be opened"
                    }.also {
                        if (it != null) {
                            latestUrl = null
                            loaded = false
                        }
                    }
                }
                if (failure != null) onEvent(PlayerEvent.Failed(failure))
            }
        }
    }

    override fun eventProperty(property: String, value: String) {}

    /**
     * At the end of an audio stream: whether it ended early, because its
     * connection dropped, and is being reopened where it stopped. A stream
     * that ends within [EARLY_END_MS] of the item's known duration, or of
     * an unknown one, really ended.
     */
    private fun streamDropped(): Boolean {
        val (audio, duration) = synchronized(lock) { playingAudio to request?.durationMs }
        if (!audio || duration == null) return false
        val at = positionMs() ?: return false
        if (duration - at <= EARLY_END_MS) return false
        return reconnect("the audio stream ended ${duration - at}ms early")
    }

    /**
     * Reopens a playing audio stream where it stopped, unless one
     * reconnect already happened without playing on [RECONNECT_AGAIN_MS]
     * since. Returns whether it did (and the failure is handled).
     */
    private fun reconnect(why: String): Boolean {
        val at = positionMs() ?: return false
        val current = synchronized(lock) {
            val again = reconnectedAtMs
            if (!playingAudio || (again != null && at - again < RECONNECT_AGAIN_MS)) return false
            reconnectedAtMs = at
            loaded = false
            request
        } ?: return false
        Log.warn("$why at ${at}ms; reconnecting")
        reopener.execute {
            synchronized(opening) {
                // Only if nothing else was loaded meanwhile.
                if (synchronized(lock) { request } !== current || destroyed) return@execute
                try {
                    open(current, at, unpause = false)
                } catch (e: PlayerError) {
                    onEvent(PlayerEvent.Failed("cannot reopen the audio stream: ${e.message}"))
                }
            }
        }
        return true
    }

    private fun addSubtitles(subtitles: List<SubtitleTrack>) {
        subtitles.forEachIndexed { i, sub ->
            // Select the first sidecar track: having one is a sign the viewer wants it.
            val flag = if (i == 0) "select" else "auto"
            try {
                mpv.command(arrayOf("sub-add", sub.url, flag, sub.title, sub.language.orEmpty()))
            } catch (e: RuntimeException) {
                Log.warn("cannot add subtitle ${sub.url}: ${e.message}")
            }
        }
    }

    private companion object {
        const val VIDEO_OUTPUT = "gpu"

        /** An audio stream ending further than this from the item's end was cut off. */
        const val EARLY_END_MS = 10_000L

        /** How far a reconnected stream must play before another reconnect is allowed. */
        const val RECONNECT_AGAIN_MS = 10_000L

        /** How far ahead to read an audio stream: about 1.4 MB at 96 kbit/s. */
        const val AUDIO_CACHE_SECS = "120"

        /** mpv options, set before it initializes. Mirrors the desktop's mpv_options. */
        val OPTIONS = listOf(
            "vo" to VIDEO_OUTPUT,
            "gpu-context" to "android",
            "opengl-es" to "yes",
            // Decode in hardware and copy back, which works with vo=gpu on
            // every device; mpv falls back to software for anything else.
            "hwdec" to "mediacodec-copy",
            "ao" to "audiotrack,opensles",
            // Stay alive with nothing loaded (between items, before tuning).
            "idle" to "yes",
            "force-window" to "no",
            // At the end of a file, pause there instead of unloading: see the file comment.
            "keep-open" to "yes",
            // Marathon is the UI.
            "osd-level" to "0",
            "input-default-bindings" to "no",
            "ytdl" to "no",
            // Sidecar subtitles come from the server (PLAN.md §2).
            "sub-auto" to "no",
            "network-timeout" to "15",
            "save-position-on-quit" to "no",
        )
    }
}
