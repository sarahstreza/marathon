// Seeking as the viewer sees it (docs/ANDROID.md "Seeking"): where a seek
// will land is shown the moment it is asked for. Kept free of mpv and
// Compose so it is unit tested.
package marathon.android

import marathon.core.AudioStream
import kotlin.math.abs

/**
 * A seek mpv has been asked for but not finished. Until playback reaches
 * it, the target stands in for the position, so the progress bar and the
 * time jump at once (a seek over the network can take a while), and a
 * second seek starts from the first one's target rather than from where
 * playback still is. Thread-safe.
 *
 * @param nowMs the current time in milliseconds.
 */
class PendingSeek(private val nowMs: () -> Long) {
    private data class Seek(val targetMs: Long, val sinceMs: Long)

    @Volatile private var seek: Seek? = null

    /** The target, while a seek is in flight; null once it landed, was cleared, or has been pending too long. */
    fun targetMs(): Long? {
        val s = seek ?: return null
        return s.targetMs.takeIf { nowMs() - s.sinceMs < GIVE_UP_MS }
    }

    /** A seek to [targetMs] was sent. */
    fun start(targetMs: Long) {
        seek = Seek(targetMs, nowMs())
    }

    /**
     * Playback is at [positionMs]: the seek has landed once that is close
     * to its target. Returns whether this landed it.
     */
    fun reached(positionMs: Long): Boolean {
        val s = seek ?: return false
        if (abs(positionMs - s.targetMs) > LANDED_MS) return false
        seek = null
        return true
    }

    /** Forget any seek (a new item was loaded). */
    fun clear() {
        seek = null
    }

    companion object {
        /** How close to the target playback must be for the seek to count as landed. */
        const val LANDED_MS = 1_500L

        /** After this long, a seek that never landed (it failed) stops standing in for the position. */
        const val GIVE_UP_MS = 10_000L

        /**
         * Where a seek of [seconds] from [positionMs] lands: never before
         * the start, nor within [AudioStream.END_MARGIN_MS] of the end when
         * [durationMs] is known (landing on the end would end the item).
         */
        fun target(positionMs: Long, seconds: Int, durationMs: Long?): Long =
            AudioStream.seekTarget(positionMs, seconds, durationMs)
    }
}
