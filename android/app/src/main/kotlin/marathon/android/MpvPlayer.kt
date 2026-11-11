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
// All mpv calls are thread-safe. load and friends run on the tuner
// thread; mpv's callbacks arrive on its event thread and only forward
// PlayerEvents to the tuner's queue.
package marathon.android

import android.content.Context
import android.view.Surface
import dev.jdtech.mpv.MPVLib
import marathon.core.Log
import marathon.core.LoadRequest
import marathon.core.Player
import marathon.core.PlayerError
import marathon.core.PlayerEvent
import marathon.core.SubtitleTrack
import java.util.Locale

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

    @Volatile private var paused = false

    private val lock = Any()

    /** The URL of the latest load, while it is pending or playing (not ended, failed, or stopped). */
    private var latestUrl: String? = null

    /** mpv has started a file since the latest load (so an idle now is about that load). */
    private var startedSinceLoad = false

    /** The latest load has opened (FILE_LOADED) and not yet ended or failed. */
    private var loaded = false

    /** Subtitles to add once the latest load has opened. */
    private var pendingSubtitles: List<SubtitleTrack> = emptyList()

    init {
        for ((name, value) in OPTIONS) mpv.setOptionString(name, value)
        mpv.init()
        for ((name, value) in headers) {
            // `append` adds one entry; setting the whole list would split values on commas.
            mpv.command(arrayOf("change-list", "http-header-fields", "append", "$name: $value"))
        }
        mpv.addObserver(this)
        mpv.observeProperty("time-pos", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
        mpv.observeProperty("duration", MPVLib.MpvFormat.MPV_FORMAT_DOUBLE)
        mpv.observeProperty("pause", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
        mpv.observeProperty("eof-reached", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
        mpv.observeProperty("idle-active", MPVLib.MpvFormat.MPV_FORMAT_FLAG)
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
        mpv.removeObserver(this)
        mpv.destroy()
    }

    // --- Player -------------------------------------------------------------

    override fun load(request: LoadRequest) {
        synchronized(lock) {
            latestUrl = request.url
            startedSinceLoad = false
            loaded = false
            pendingSubtitles = request.subtitles
        }
        timePos = null
        duration = null
        // `start` applies to the next file loaded; it is set before every
        // load, so it never leaks between items.
        mpv.setPropertyString("start", String.format(Locale.ROOT, "%.3f", request.startMs / 1000.0))
        mpv.setPropertyBoolean("pause", false)
        paused = false
        try {
            mpv.command(arrayOf("loadfile", request.url, "replace"))
        } catch (e: RuntimeException) {
            throw PlayerError(e.message ?: e.toString())
        }
    }

    override fun stop() {
        synchronized(lock) {
            latestUrl = null
            loaded = false
        }
        mpv.command(arrayOf("stop"))
    }

    override fun setPaused(paused: Boolean) {
        mpv.setPropertyBoolean("pause", paused)
        this.paused = paused
    }

    override fun isPaused(): Boolean = paused

    override fun seekRelative(seconds: Int) {
        mpv.command(arrayOf("seek", seconds.toString(), "relative"))
    }

    override fun positionMs(): Long? = timePos?.takeIf { it.isFinite() && it >= 0 }?.let { Math.round(it * 1000) }

    override fun durationMs(): Long? = duration?.takeIf { it.isFinite() && it > 0 }?.let { Math.round(it * 1000) }

    override fun toggleSubtitles() {
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
        }
    }

    override fun eventProperty(property: String, value: Long) {}

    override fun eventProperty(property: String, value: Double) {
        when (property) {
            "time-pos" -> timePos = value
            "duration" -> duration = value
        }
    }

    override fun eventProperty(property: String, value: Boolean) {
        when (property) {
            "pause" -> paused = value
            "eof-reached" -> if (value) {
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
