// A port of the tests in crates/marathon-client/src/navigation.rs.
@file:Suppress("FunctionName")

package marathon.core

import marathon.api.models.ChannelSummary
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class NavigationTest {
    private fun channels(n: Int) = (1..n).map { i ->
        ChannelSummary(id = "c$i", itemCount = 1, name = "Channel $i", number = i, positionMs = 0)
    }

    @Test
    fun with_nothing_open_up_down_change_channel_and_left_right_seek() {
        val nav = Navigation()
        val ch = channels(3)
        assertEquals(Resolved.Do(Action.ChannelUp), nav.navigate(Direction.Up, ch))
        assertEquals(Resolved.Do(Action.ChannelDown), nav.navigate(Direction.Down, ch))
        assertEquals(Resolved.Do(Action.SeekRelative(-10)), nav.navigate(Direction.Left, ch))
        assertEquals(Resolved.Do(Action.SeekRelative(10)), nav.navigate(Direction.Right, ch))
        assertEquals(Resolved.Do(Action.ShowInfo), nav.select(ch))
        assertFalse(nav.close())
    }

    @Test
    fun with_the_guide_open_arrows_move_the_selection() {
        val ch = channels(3)
        val nav = Navigation()
        nav.openGuide(ch, "c2")
        assertEquals(1, nav.highlighted())

        assertEquals(Resolved.GuideMoved, nav.navigate(Direction.Down, ch))
        assertEquals(2, nav.highlighted())
        assertEquals(Resolved.GuideMoved, nav.navigate(Direction.Down, ch))
        assertEquals(0, nav.highlighted(), "wraps at the bottom")
        assertEquals(Resolved.GuideMoved, nav.navigate(Direction.Up, ch))
        assertEquals(2, nav.highlighted(), "wraps at the top")
        assertEquals(Resolved.Nothing, nav.navigate(Direction.Left, ch))
        assertEquals(Resolved.Nothing, nav.navigate(Direction.Right, ch))
        assertEquals(2, nav.highlighted())

        assertEquals(Resolved.Do(Action.GoToChannelId("c3")), nav.select(ch))
        assertFalse(nav.guideOpen(), "selecting closes the guide")
    }

    @Test
    fun back_closes_the_guide() {
        val ch = channels(2)
        val nav = Navigation()
        nav.openGuide(ch, null)
        assertEquals(0, nav.highlighted())
        assertTrue(nav.close())
        assertFalse(nav.guideOpen())
        assertEquals(Resolved.Do(Action.ChannelUp), nav.navigate(Direction.Up, ch))
    }

    @Test
    fun an_empty_guide_never_panics() {
        val nav = Navigation()
        nav.openGuide(emptyList(), null)
        for (d in Direction.entries) assertEquals(Resolved.Nothing, nav.navigate(d, emptyList()))
        assertEquals(Resolved.Nothing, nav.select(emptyList()))
    }
}
