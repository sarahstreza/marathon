// A port of the state tests in apps/marathon-desktop/src/overlay/mod.rs.
@file:Suppress("FunctionName")

package marathon.core.overlay

import marathon.core.Banner
import marathon.core.Guide
import marathon.core.GuideRow
import marathon.core.UiEvent
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class OverlayModelTest {
    private val t0 = 1_000_000L

    private fun banner(n: Int) = Banner(channelNumber = n, channelName = "Channel $n", title = "S01E01 · Pilot", show = null)

    private fun guide() = Guide(rows = listOf(GuideRow(number = 1, name = "One", on = null, current = true)), highlighted = 0)

    @Test
    fun tuning_shows_the_banner_at_once_and_it_goes_away_by_itself() {
        val o = OverlayModel()
        o.cue(UiEvent.Tuning(banner(2)), t0)
        assertTrue(o.frame(t0).tuning)
        assertEquals(Wake.Animating, o.frame(t0).wake)
        assertEquals(1f, o.frame(t0 + 150).bannerOpacity)
        o.cue(UiEvent.Playing(banner(2), announce = true), t0 + 400)
        assertFalse(o.frame(t0 + 400).tuning)
        val gone = t0 + 400 + BANNER_HOLD_MS + 1_000
        assertEquals(0f, o.frame(gone).bannerOpacity)
        assertFalse(o.frame(gone).visible)
        assertEquals(Wake.Idle, o.frame(gone).wake)
    }

    @Test
    fun automatic_advances_do_not_pop_the_banner_up() {
        val o = OverlayModel()
        o.cue(UiEvent.Playing(banner(1), announce = false), t0)
        assertEquals(0f, o.frame(t0 + 200).bannerOpacity)
        assertEquals(banner(1), o.frame(t0).banner)
        // ...but asking for info shows what is on now.
        o.cue(UiEvent.ShowInfo, t0 + 300)
        assertEquals(1f, o.frame(t0 + 500).bannerOpacity)
    }

    @Test
    fun the_guide_stays_until_closed_and_hides_the_banner() {
        val o = OverlayModel()
        o.cue(UiEvent.Tuning(banner(1)), t0)
        o.cue(UiEvent.GuideChanged(guide()), t0 + 200)
        assertTrue(o.guideShown(t0 + 200))
        assertEquals(0f, o.frame(t0 + 600).bannerOpacity, "the banner gives way to the guide")
        assertEquals(1f, o.frame(t0 + 60_000).guideOpacity, "no timeout")
        // Info while the guide is open does not cover it.
        o.cue(UiEvent.ShowInfo, t0 + 60_000)
        assertEquals(0f, o.frame(t0 + 60_200).bannerOpacity)
        o.cue(UiEvent.GuideChanged(null), t0 + 61_000)
        assertFalse(o.guideShown(t0 + 61_000))
        assertEquals(0f, o.frame(t0 + 61_300).guideOpacity)
    }

    @Test
    fun toasts_show_and_back_hides_everything() {
        val o = OverlayModel()
        o.cue(UiEvent.Tuning(banner(1)), t0)
        o.toast("cannot reach the server", t0)
        assertEquals("cannot reach the server", o.frame(t0).toast)
        assertEquals(1f, o.frame(t0 + 150).toastOpacity)
        o.cue(UiEvent.Hide, t0 + 1_000)
        val later = o.frame(t0 + 1_300)
        assertEquals(0f, later.bannerOpacity)
        assertEquals(0f, later.toastOpacity)
    }
}
