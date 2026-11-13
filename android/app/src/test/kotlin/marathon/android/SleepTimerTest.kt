// The sleep timer's durations, warning, and expiry, against a fake clock
// and scheduler (as InputTest drives KeyInput).
@file:Suppress("FunctionName")

package marathon.android

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class SleepTimerTest {
    private class Harness {
        var now = 1_000L
        val scheduled = mutableListOf<Pair<Long, () -> Unit>>()
        val warnings = mutableListOf<Long>()
        var slept = 0
        val timer = SleepTimer(
            schedule = { delay, block ->
                val entry = (now + delay) to block
                scheduled.add(entry)
                ({ scheduled.remove(entry) })
            },
            nowMs = { now },
            onWarn = { warnings.add(it) },
            onSleep = { slept += 1 },
        )

        fun advance(ms: Long) {
            now += ms
            scheduled.filter { it.first <= now }.sortedBy { it.first }.forEach {
                if (scheduled.remove(it)) it.second()
            }
        }
    }

    private val minute = 60_000L

    @Test
    fun cycles_through_the_durations_then_off() {
        val h = Harness()
        val seen = (1..SleepTimer.DURATIONS_MIN.size + 1).map { h.timer.cycle() }
        assertEquals(SleepTimer.DURATIONS_MIN + listOf(null), seen)
        assertNull(h.timer.remainingMs())
        assertTrue(h.scheduled.isEmpty(), "off cancels everything")
        assertEquals(15, h.timer.cycle(), "and round again")
    }

    @Test
    fun warns_a_minute_before_then_sleeps_once() {
        val h = Harness()
        h.timer.cycle()
        assertEquals(15 * minute, h.timer.remainingMs())
        h.advance(14 * minute - 1)
        assertEquals(emptyList<Long>(), h.warnings)
        h.advance(1)
        assertEquals(listOf(SleepTimer.WARNING_MS), h.warnings)
        assertEquals(0, h.slept)
        h.advance(minute)
        assertEquals(1, h.slept)
        assertNull(h.timer.remainingMs(), "off once it has fired")
        assertEquals(15, h.timer.cycle(), "the next press starts over")
    }

    @Test
    fun choosing_again_restarts_from_now() {
        val h = Harness()
        h.timer.cycle()
        h.advance(10 * minute)
        assertEquals(30, h.timer.cycle())
        assertEquals(30 * minute, h.timer.remainingMs())
        h.advance(20 * minute)
        assertEquals(0, h.slept, "the 15-minute timer was replaced")
        assertEquals(2, h.scheduled.size)
    }

    @Test
    fun cancel_stops_it() {
        val h = Harness()
        h.timer.cycle()
        h.timer.cancel()
        h.advance(60 * minute)
        assertEquals(0, h.slept)
        assertEquals(emptyList<Long>(), h.warnings)
    }

    @Test
    fun label_rounds_minutes_up() {
        assertEquals("Sleep", SleepTimer.label(null))
        assertEquals("Sleep 30m", SleepTimer.label(30 * minute))
        assertEquals("Sleep 30m", SleepTimer.label(29 * minute + 1))
        assertEquals("Sleep 1m", SleepTimer.label(1))
    }
}
