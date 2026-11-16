// Seeking as the viewer sees it (docs/ANDROID.md "Seeking"): where a seek
// will land is shown the moment it is asked for, and the progress bar can
// be dragged to scrub. Pure arithmetic, kept free of mpv and Compose so it
// is unit tested.
package marathon.android

import marathon.core.AudioStream
import kotlin.math.abs
import kotlin.math.roundToInt

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

/**
 * One drag along the progress bar. Dragging the full width of the bar
 * moves through the whole item, but sliding the finger up off the bar
 * makes the same drag move less, for fine positioning: half as far, a
 * quarter, then an eighth, in bands [BAND_DP] tall. Moves are relative, so
 * changing band never makes the target jump.
 *
 * @param startMs the position when the drag began.
 * @param durationMs the item's duration.
 * @param barWidthPx the progress bar's width.
 * @param pxPerDp the screen density.
 */
class Scrub(startMs: Long, private val durationMs: Long, private val barWidthPx: Float, private val pxPerDp: Float) {
    /** Where the drag has got to, in the item. */
    var targetMs: Long = clamp(startMs)
        private set

    /** How far a drag moves now, as a fraction of full speed. */
    var speed: Float = 1f
        private set

    /** The finger moved [dxPx] sideways, and is [abovePx] above the bar's middle (negative: below). */
    fun move(dxPx: Float, abovePx: Float) {
        speed = speedAt(abovePx / pxPerDp)
        if (barWidthPx <= 0f) return
        val deltaMs = dxPx / barWidthPx * durationMs * speed
        targetMs = clamp(targetMs + deltaMs.toLong())
    }

    /** The relative seek, in whole seconds, from [positionMs] to the target. */
    fun secondsFrom(positionMs: Long): Int = ((targetMs - positionMs) / 1000.0).roundToInt()

    private fun clamp(ms: Long): Long = ms.coerceIn(0, (durationMs - AudioStream.END_MARGIN_MS).coerceAtLeast(0))

    companion object {
        /** How tall each band of finer scrubbing is. */
        const val BAND_DP = 48f

        /** The speeds of the bands, from on the bar upwards. */
        private val SPEEDS = floatArrayOf(1f, 0.5f, 0.25f, 0.125f)

        fun speedAt(aboveDp: Float): Float {
            val band = if (aboveDp <= 0f) 0 else (aboveDp / BAND_DP).toInt()
            return SPEEDS[band.coerceAtMost(SPEEDS.size - 1)]
        }

        /** What to call [speed] next to the time, or null at full speed. */
        fun speedLabel(speed: Float): String? = when {
            speed >= 1f -> null
            speed >= 0.5f -> "Half speed"
            speed >= 0.25f -> "Quarter speed"
            else -> "Fine"
        }
    }
}
