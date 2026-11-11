// A port of the tests in apps/marathon-desktop/src/overlay/fade.rs: fade
// timing is tested independently of rendering (M5).
@file:Suppress("FunctionName")

package marathon.core.overlay

import kotlin.math.abs
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class FadeTest {
    private val hold = 4_000L
    private val t0 = 1_000_000L

    private fun close(a: Float, b: Float) = abs(a - b) < 0.02f

    @Test
    fun hidden_until_shown() {
        val fade = Fade()
        assertEquals(0f, fade.opacity(t0))
        assertEquals(Wake.Idle, fade.wake(t0))
        assertFalse(fade.isShown(t0))
    }

    @Test
    fun fades_in_holds_and_fades_out() {
        val fade = Fade()
        fade.show(t0, hold)
        assertEquals(0f, fade.opacity(t0))
        assertEquals(Wake.Animating, fade.wake(t0))
        val midIn = fade.opacity(t0 + 75)
        assertTrue(midIn > 0.5f && midIn < 1f, "$midIn")
        assertEquals(1f, fade.opacity(t0 + 150))

        val outAt = t0 + 150 + hold
        assertEquals(1f, fade.opacity(t0 + 2_000))
        assertEquals(Wake.At(outAt), fade.wake(t0 + 2_000))
        assertTrue(fade.isShown(t0 + 2_000))

        assertEquals(1f, fade.opacity(outAt))
        assertEquals(Wake.Animating, fade.wake(outAt))
        assertFalse(fade.isShown(outAt))
        assertTrue(close(fade.opacity(outAt + 150), 0.5f))
        assertEquals(0f, fade.opacity(outAt + 300))
        assertEquals(Wake.Idle, fade.wake(outAt + 300))
    }

    @Test
    fun opacity_is_monotonic_through_each_phase() {
        val fade = Fade()
        fade.show(t0, hold)
        val rising = (0..150).map { fade.opacity(t0 + it) }
        assertTrue(rising.zipWithNext().all { (a, b) -> a <= b }, "fade-in rises")
        val outAt = t0 + 150 + hold
        val falling = (0..300).map { fade.opacity(outAt + it) }
        assertTrue(falling.zipWithNext().all { (a, b) -> a >= b }, "fade-out falls")
    }

    @Test
    fun showing_again_while_visible_extends_the_hold_without_a_flicker() {
        val fade = Fade()
        fade.show(t0, hold)
        val t1 = t0 + 2_000
        fade.show(t1, hold)
        assertEquals(1f, fade.opacity(t1))
        assertEquals(Wake.At(t1 + hold), fade.wake(t1))
    }

    @Test
    fun showing_during_the_fade_out_fades_back_in_from_there() {
        val fade = Fade()
        fade.show(t0, hold)
        val t1 = t0 + 150 + hold + 150
        val mid = fade.opacity(t1)
        assertTrue(close(mid, 0.5f), "$mid")
        fade.show(t1, hold)
        assertTrue(close(fade.opacity(t1), mid), "no jump")
        assertTrue(fade.opacity(t1 + 30) > mid)
        // Half way up, the fade-in takes half the time.
        assertEquals(Wake.At(t1 + 75 + hold), fade.wake(t1 + 75))
    }

    @Test
    fun hide_fades_out_from_where_it_is() {
        val fade = Fade()
        fade.show(t0, null)
        assertEquals(Wake.Idle, fade.wake(t0 + 500), "held until hidden")
        fade.hide(t0 + 1_000)
        assertFalse(fade.isShown(t0 + 1_000))
        assertEquals(0f, fade.opacity(t0 + 1_300))
    }

    @Test
    fun wakes_combine_to_the_most_urgent() {
        assertEquals(Wake.Animating, Wake.At(5).min(Wake.Animating))
        assertEquals(Wake.At(3), Wake.At(5).min(Wake.At(3)))
        assertEquals(Wake.At(5), Wake.Idle.min(Wake.At(5)))
        assertEquals(Wake.Idle, Wake.Idle.min(Wake.Idle))
    }
}
