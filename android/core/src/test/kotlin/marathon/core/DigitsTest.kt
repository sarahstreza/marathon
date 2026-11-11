// A port of the tests in crates/marathon-client/src/digits.rs.
@file:Suppress("FunctionName")

package marathon.core

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class DigitsTest {
    @Test
    fun digits_accumulate_commit_and_cap() {
        val now = 1_000L
        val entry = ChannelNumberEntry()
        assertTrue(entry.isEmpty())
        assertNull(entry.commit())
        for (d in listOf(1, 2, 3, 4, 5)) entry.digit(d, now)
        assertEquals("1234", entry.pending())
        assertEquals(Action.GoToChannel(1234), entry.commit())
        assertTrue(entry.isEmpty())
        assertNull(entry.deadline())
    }

    @Test
    fun the_timeout_commits_and_clear_discards() {
        val t0 = 1_000L
        val entry = ChannelNumberEntry()
        entry.digit(4, t0)
        entry.digit(2, t0 + DIGIT_TIMEOUT_MS / 2)
        val deadline = entry.deadline()!!
        assertEquals(t0 + DIGIT_TIMEOUT_MS / 2 + DIGIT_TIMEOUT_MS, deadline)
        assertNull(entry.tick(deadline - 1))
        assertEquals(Action.GoToChannel(42), entry.tick(deadline))
        entry.digit(7, t0)
        entry.clear()
        assertNull(entry.tick(t0 + DIGIT_TIMEOUT_MS * 2))
    }
}
