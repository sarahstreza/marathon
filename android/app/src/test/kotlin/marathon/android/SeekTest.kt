// Seeking as the viewer sees it: a pending seek standing in for the
// position.
@file:Suppress("FunctionName")

package marathon.android

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class SeekTest {
    private var now = 0L
    private val pending = PendingSeek { now }

    @Test
    fun `a pending seek stands in for the position until playback reaches it`() {
        pending.start(600_000)
        assertEquals(600_000, pending.targetMs())
        // Still where it was: the seek hasn't happened yet.
        assertFalse(pending.reached(30_000))
        assertEquals(600_000, pending.targetMs())
        assertTrue(pending.reached(599_200))
        assertNull(pending.targetMs())
    }

    @Test
    fun `a seek that never lands stops standing in after a while`() {
        pending.start(600_000)
        now += PendingSeek.GIVE_UP_MS - 1
        assertEquals(600_000, pending.targetMs())
        now += 1
        assertNull(pending.targetMs())
    }

    @Test
    fun `a new seek replaces the pending one, and clearing forgets it`() {
        pending.start(600_000)
        pending.start(610_000)
        assertFalse(pending.reached(600_000))
        assertEquals(610_000, pending.targetMs())
        pending.clear()
        assertNull(pending.targetMs())
    }

    @Test
    fun `seek targets stay inside the item`() {
        assertEquals(0, PendingSeek.target(5_000, -10, 1_500_000))
        assertEquals(1_499_000, PendingSeek.target(1_495_000, 10, 1_500_000))
        assertEquals(40_000, PendingSeek.target(30_000, 10, null))
    }
}
