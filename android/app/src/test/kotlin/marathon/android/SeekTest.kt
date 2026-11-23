// Seeking as the viewer sees it: a pending seek standing in for the
// position, and scrubbing along the progress bar.
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

    @Test
    fun `dragging the width of the bar crosses the whole item`() {
        val scrub = Scrub(startMs = 0, durationMs = 1_000_000, barWidthPx = 1_000f, pxPerDp = 2f)
        scrub.move(dxPx = 250f, abovePx = 0f)
        assertEquals(250_000, scrub.targetMs)
        assertEquals(1f, scrub.speed)
        scrub.move(dxPx = -100f, abovePx = 0f)
        assertEquals(150_000, scrub.targetMs)
    }

    @Test
    fun `sliding up off the bar makes the same drag move less`() {
        val pxPerDp = 2f
        fun dragAt(aboveDp: Float): Long {
            val scrub = Scrub(startMs = 500_000, durationMs = 1_000_000, barWidthPx = 1_000f, pxPerDp = pxPerDp)
            scrub.move(dxPx = 100f, abovePx = aboveDp * pxPerDp)
            return scrub.targetMs - 500_000
        }
        assertEquals(100_000, dragAt(-30f)) // Below the bar: full speed.
        assertEquals(100_000, dragAt(Scrub.BAND_DP - 1))
        assertEquals(50_000, dragAt(Scrub.BAND_DP))
        assertEquals(25_000, dragAt(Scrub.BAND_DP * 2))
        assertEquals(12_500, dragAt(Scrub.BAND_DP * 3))
        assertEquals(12_500, dragAt(Scrub.BAND_DP * 10))
    }

    @Test
    fun `changing speed doesn't make the target jump`() {
        val scrub = Scrub(startMs = 500_000, durationMs = 1_000_000, barWidthPx = 1_000f, pxPerDp = 1f)
        scrub.move(dxPx = 0f, abovePx = 200f)
        assertEquals(500_000, scrub.targetMs)
        assertEquals("Fine", Scrub.speedLabel(scrub.speed))
    }

    @Test
    fun `a scrub stays inside the item`() {
        val scrub = Scrub(startMs = 10_000, durationMs = 1_000_000, barWidthPx = 1_000f, pxPerDp = 1f)
        scrub.move(dxPx = -500f, abovePx = 0f)
        assertEquals(0, scrub.targetMs)
        scrub.move(dxPx = 5_000f, abovePx = 0f)
        assertEquals(999_000, scrub.targetMs)
    }

    @Test
    fun `the seek is relative to where playback has got to, in whole seconds`() {
        val scrub = Scrub(startMs = 100_000, durationMs = 1_000_000, barWidthPx = 1_000f, pxPerDp = 1f)
        scrub.move(dxPx = 100f, abovePx = 0f) // To 200 000.
        assertEquals(97, scrub.secondsFrom(positionMs = 103_400))
        assertEquals(-3, scrub.secondsFrom(positionMs = 203_000))
    }

    @Test
    fun `speed labels`() {
        assertNull(Scrub.speedLabel(1f))
        assertEquals("Half speed", Scrub.speedLabel(0.5f))
        assertEquals("Quarter speed", Scrub.speedLabel(0.25f))
        assertEquals("Fine", Scrub.speedLabel(0.125f))
    }
}
