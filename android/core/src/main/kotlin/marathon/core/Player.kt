// The Player interface: what the Tuner needs from a video player. A port
// of crates/marathon-client/src/player.rs. The app implements it with
// libmpv (MpvPlayer); tests use a mock.
package marathon.core

/** A sidecar subtitle to load alongside a video. */
data class SubtitleTrack(
    /** Absolute URL of the subtitle file. */
    val url: String,
    /** Track title to show, e.g. `en.forced`. May be empty. */
    val title: String,
    val language: String?,
)

/** What to play. */
data class LoadRequest(
    /** Absolute URL of the video stream. */
    val url: String,
    /** Where to start, in milliseconds. */
    val startMs: Long,
    /** Sidecar subtitles to add once the video has loaded. */
    val subtitles: List<SubtitleTrack>,
    /**
     * Where to stream just the item's audio from, if the server offers it
     * (`Now.audio_url`, PLAN.md M12). A player in audio-only mode plays
     * this instead of [url]; see [AudioStream].
     */
    val audioUrl: String? = null,
    /**
     * The item's duration as the server knows it. An audio stream carries
     * none, so an audio-only player reports this one.
     */
    val durationMs: Long? = null,
)

/** A player failure that is not about one particular file. */
class PlayerError(message: String) : Exception(message) {
    override fun toString(): String = "player error: $message"
}

/** Something the player reports back to the tuner. */
sealed interface PlayerEvent {
    /**
     * The most recently loaded file opened and started playing. Ends a
     * streak of failures.
     */
    data object Started : PlayerEvent

    /**
     * The most recently loaded file played to its end. Must not be sent for
     * files that were replaced or stopped, nor for any file but the latest
     * loaded one.
     */
    data object Ended : PlayerEvent

    /** The most recently loaded file could not be opened or played. The tuner skips ahead. */
    data class Failed(val reason: String) : PlayerEvent
}

/**
 * A video player the tuner drives. Implementations play one file at a
 * time; [load] replaces whatever is playing and starts playback (unpaused).
 */
interface Player {
    /** Replaces the current file with [request] and plays it from `request.startMs`. */
    @Throws(PlayerError::class)
    fun load(request: LoadRequest)

    /** Stops playback and unloads the file. */
    fun stop()

    fun setPaused(paused: Boolean)

    fun isPaused(): Boolean

    /** Seeks by [seconds] (negative is backwards) from the current position. */
    fun seekRelative(seconds: Int)

    /**
     * Playback position in the current file, or null if nothing is loaded
     * yet (e.g. still opening the stream).
     */
    fun positionMs(): Long?

    /** Duration of the current file, if known. */
    fun durationMs(): Long?

    /** Turns subtitles on or off. */
    fun toggleSubtitles()
}
