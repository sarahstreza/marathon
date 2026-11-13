// The sleep timer (docs/ANDROID.md "Sleep timer"): pause playback after a
// while, for falling asleep in front of the TV. Each press of the control
// strip's Sleep button steps through the durations and back to off. A
// minute before the end it warns, then it calls onSleep, which pauses and
// lets the screen turn off.
//
// Android-only for now: PLAN.md lists a sleep timer after the MVP, and
// there is no Action for it, so a remote or the HTTP API can't set it.
package marathon.android

/**
 * @param schedule runs a callback after a delay (on the caller's thread); returns a canceller.
 * @param nowMs the current time in milliseconds.
 * @param onWarn called [WARNING_MS] before the end, with the time left.
 * @param onSleep called when the time is up.
 */
class SleepTimer(
    private val schedule: (delayMs: Long, block: () -> Unit) -> () -> Unit,
    private val nowMs: () -> Long,
    private val onWarn: (remainingMs: Long) -> Unit,
    private val onSleep: () -> Unit,
) {
    /** The duration last chosen, in minutes, while the timer runs. */
    private var minutes: Int? = null
    private var endsAt: Long? = null
    private var cancels: List<() -> Unit> = emptyList()

    /** Time left, or null when the timer is off. */
    fun remainingMs(): Long? = endsAt?.let { maxOf(0, it - nowMs()) }

    /**
     * Steps to the next duration (off → 15 → 30 → 45 → 60 → 90 → 120 min
     * → off), starting it from now. Returns the new duration in minutes,
     * or null for off.
     */
    fun cycle(): Int? {
        val next = when (val current = minutes) {
            null -> DURATIONS_MIN.first()
            else -> DURATIONS_MIN.firstOrNull { it > current }
        }
        if (next == null) cancel() else start(next)
        return next
    }

    /** Turns the timer off. */
    fun cancel() {
        cancels.forEach { it() }
        cancels = emptyList()
        minutes = null
        endsAt = null
    }

    private fun start(minutes: Int) {
        cancel()
        val durationMs = minutes * 60_000L
        this.minutes = minutes
        endsAt = nowMs() + durationMs
        cancels = listOf(
            schedule(durationMs - WARNING_MS) { onWarn(WARNING_MS) },
            schedule(durationMs) {
                this.minutes = null
                endsAt = null
                cancels = emptyList()
                onSleep()
            },
        )
    }

    companion object {
        /** The durations the Sleep button steps through, in minutes. */
        val DURATIONS_MIN = listOf(15, 30, 45, 60, 90, 120)

        /** How long before the end the viewer is warned. */
        const val WARNING_MS = 60_000L

        /** "Sleep" when off, else the minutes left, rounded up: "Sleep 29m". */
        fun label(remainingMs: Long?): String =
            if (remainingMs == null) "Sleep" else "Sleep ${(remainingMs + 59_999) / 60_000}m"
    }
}
