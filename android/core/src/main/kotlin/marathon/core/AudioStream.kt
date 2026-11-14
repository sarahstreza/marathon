// Audio-only playback (PLAN.md M12): the arithmetic a player needs to play
// the server's audio streams (`GET /audio/{id}?start_ms=`). Kept here,
// free of mpv, so it is unit tested.
//
// An audio stream is piped out of ffmpeg on the server: its timestamps
// start at zero wherever it starts, it has no duration, and it can't be
// seeked by byte ranges. So a player asks for the stream from the position
// it wants, adds that offset to the positions it reports, takes the
// duration from the server, and seeks by asking for a new stream.
package marathon.core

object AudioStream {
    /**
     * How close to the end a seek may land. Starting a stream at or past
     * its end gives nothing to play, which would count as a failure.
     */
    const val END_MARGIN_MS = 1_000L

    /** [audioUrl] (absolute, from `Now.audio_url`) starting at [startMs]. */
    fun url(audioUrl: String, startMs: Long): String {
        val separator = if ('?' in audioUrl) '&' else '?'
        return "$audioUrl${separator}start_ms=${startMs.coerceAtLeast(0)}"
    }

    /**
     * The position in the item, given the stream started at [offsetMs] and
     * the player is [streamSeconds] into it (null or invalid: not yet known).
     */
    fun positionMs(offsetMs: Long, streamSeconds: Double?): Long? {
        val seconds = streamSeconds?.takeIf { it.isFinite() && it >= 0 } ?: return null
        return offsetMs + Math.round(seconds * 1000)
    }

    /**
     * Where a relative seek of [seconds] from [positionMs] lands: never
     * before the start, nor within [END_MARGIN_MS] of the end when
     * [durationMs] is known.
     */
    fun seekTarget(positionMs: Long, seconds: Int, durationMs: Long?): Long {
        val target = positionMs + seconds * 1000L
        val latest = durationMs?.let { (it - END_MARGIN_MS).coerceAtLeast(0) } ?: Long.MAX_VALUE
        return target.coerceIn(0, latest)
    }
}
