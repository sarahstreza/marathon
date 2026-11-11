// Fade timing for overlay elements: opacity as a function of time,
// independent of rendering (PLAN.md §9: fade in ~150ms, hold ~4s, fade out
// ~300ms, eased). A port of apps/marathon-desktop/src/overlay/fade.rs, so
// the Android overlay moves exactly like the desktop one. Times are Clock
// milliseconds.
//
// A Fade is shown, optionally for a limited hold, and hidden. Showing
// again while visible or fading out continues from the current opacity
// rather than popping, and only extends the hold of a fully visible
// element.
package marathon.core.overlay

import kotlin.math.pow
import kotlin.math.roundToLong

/** How long fading takes. */
data class Timing(val fadeInMs: Long, val fadeOutMs: Long) {
    companion object {
        /** PLAN.md §9's banner timing. */
        val STANDARD = Timing(fadeInMs = 150, fadeOutMs = 300)
    }
}

/** Whether, and how soon, a fade needs drawing again. */
sealed interface Wake {
    /** Opacity is changing: draw every frame. */
    data object Animating : Wake

    /** Opacity is steady until this time. */
    data class At(val ms: Long) : Wake

    /** Opacity is steady until something calls show or hide. */
    data object Idle : Wake

    /** The more urgent of two wakes. */
    fun min(other: Wake): Wake = when {
        this == Animating || other == Animating -> Animating
        this is At && other is At -> At(minOf(ms, other.ms))
        this is At -> this
        other is At -> other
        else -> Idle
    }
}

/** One overlay element's fade. */
class Fade(private val timing: Timing = Timing.STANDARD) {
    private class Shown(
        /** When the fade-in started, the opacity it started from, and how long it takes. */
        val at: Long,
        val from: Float,
        val fadeInMs: Long,
        /** When the fade-out starts, if scheduled, and the opacity it starts from. */
        var hideAt: Long?,
        var hideFrom: Float,
    )

    private var shown: Shown? = null

    /**
     * Shows the element at [now]: fades it in (from wherever it is), then
     * holds it for [holdMs], or until [hide] if null.
     */
    fun show(now: Long, holdMs: Long?) {
        val current = opacity(now).coerceIn(0f, 1f)
        // From part way up, the fade-in takes its share of the full time, so
        // it moves at the usual speed.
        val fadeIn = (timing.fadeInMs * (1.0 - current)).roundToLong()
        shown = Shown(at = now, from = current, fadeInMs = fadeIn, hideAt = holdMs?.let { now + fadeIn + it }, hideFrom = 1f)
    }

    /** Starts fading out at [now], from the current opacity. */
    fun hide(now: Long) {
        val s = shown ?: return
        val current = opacity(now)
        val hideAt = s.hideAt
        when {
            current <= 0f -> shown = null
            // Not already fading out: start now.
            hideAt == null || hideAt > now -> {
                s.hideAt = now
                s.hideFrom = current
            }
        }
    }

    /** Whether the element is shown and not fading out (it may be fading in). */
    fun isShown(now: Long): Boolean {
        val s = shown ?: return false
        val hideAt = s.hideAt
        return hideAt == null || now < hideAt
    }

    /** Opacity at [now], from 0 (invisible) to 1. */
    fun opacity(now: Long): Float {
        val s = shown ?: return 0f
        val hideAt = s.hideAt
        if (hideAt != null && now >= hideAt) {
            val t = fraction(now - hideAt, timing.fadeOutMs)
            return s.hideFrom * (1f - easeInOut(t))
        }
        val t = fraction(maxOf(0, now - s.at), s.fadeInMs)
        return s.from + (1f - s.from) * easeOut(t)
    }

    /** When this element next needs drawing. */
    fun wake(now: Long): Wake {
        val s = shown ?: return Wake.Idle
        val hideAt = s.hideAt
        return when {
            hideAt != null && now >= hideAt -> if (now < hideAt + timing.fadeOutMs) Wake.Animating else Wake.Idle
            now < s.at + s.fadeInMs -> Wake.Animating
            hideAt != null -> Wake.At(hideAt)
            else -> Wake.Idle
        }
    }
}

/** `elapsed / total`, clamped to 0..1 (1 if total is zero). */
private fun fraction(elapsed: Long, total: Long): Float =
    if (total <= 0) 1f else (elapsed.toFloat() / total.toFloat()).coerceIn(0f, 1f)

/** Cubic ease-out: fast start, gentle landing. */
private fun easeOut(t: Float): Float = 1f - (1f - t).pow(3)

/** Cubic ease-in-out. */
private fun easeInOut(t: Float): Float = if (t < 0.5f) 4f * t * t * t else 1f - (-2f * t + 2f).pow(3) / 2f
