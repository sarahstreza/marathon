// Every remote/keyboard binding and touch gesture produces the expected
// Action (the Android counterpart of the desktop's keyboard and gamepad
// mapping tests, M3/M5). Plain JVM tests: KeyEvent's key codes are
// compile-time constants.
@file:Suppress("FunctionName")

package marathon.android

import android.view.KeyEvent
import marathon.core.Action
import marathon.core.DIGIT_TIMEOUT_MS
import marathon.core.Direction
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class InputTest {
    private fun act(action: Action, repeats: Boolean = false) = KeyCommand.Act(action, repeats)

    @Test
    fun every_key_binding_maps_to_its_action() {
        val expected = mapOf(
            KeyEvent.KEYCODE_DPAD_UP to act(Action.Navigate(Direction.Up)),
            KeyEvent.KEYCODE_DPAD_DOWN to act(Action.Navigate(Direction.Down)),
            KeyEvent.KEYCODE_DPAD_LEFT to act(Action.Navigate(Direction.Left), repeats = true),
            KeyEvent.KEYCODE_DPAD_RIGHT to act(Action.Navigate(Direction.Right), repeats = true),
            KeyEvent.KEYCODE_DPAD_CENTER to KeyCommand.Enter,
            KeyEvent.KEYCODE_ENTER to KeyCommand.Enter,
            KeyEvent.KEYCODE_BACK to KeyCommand.Back,
            KeyEvent.KEYCODE_ESCAPE to KeyCommand.Back,
            KeyEvent.KEYCODE_CHANNEL_UP to act(Action.ChannelUp),
            KeyEvent.KEYCODE_CHANNEL_DOWN to act(Action.ChannelDown),
            KeyEvent.KEYCODE_PAGE_UP to act(Action.ChannelUp),
            KeyEvent.KEYCODE_PAGE_DOWN to act(Action.ChannelDown),
            KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE to act(Action.TogglePause),
            KeyEvent.KEYCODE_SPACE to act(Action.TogglePause),
            KeyEvent.KEYCODE_MEDIA_PLAY to act(Action.Play),
            KeyEvent.KEYCODE_MEDIA_PAUSE to act(Action.Pause),
            KeyEvent.KEYCODE_MEDIA_STOP to act(Action.Pause),
            KeyEvent.KEYCODE_MEDIA_REWIND to act(Action.SeekRelative(-10), repeats = true),
            KeyEvent.KEYCODE_MEDIA_FAST_FORWARD to act(Action.SeekRelative(10), repeats = true),
            KeyEvent.KEYCODE_MEDIA_NEXT to act(Action.NextItem),
            KeyEvent.KEYCODE_MEDIA_PREVIOUS to act(Action.PreviousItem),
            KeyEvent.KEYCODE_N to act(Action.NextItem),
            KeyEvent.KEYCODE_P to act(Action.PreviousItem),
            KeyEvent.KEYCODE_INFO to act(Action.ShowInfo),
            KeyEvent.KEYCODE_I to act(Action.ShowInfo),
            KeyEvent.KEYCODE_MENU to act(Action.ShowInfo),
            KeyEvent.KEYCODE_GUIDE to act(Action.ToggleGuide),
            KeyEvent.KEYCODE_G to act(Action.ToggleGuide),
            KeyEvent.KEYCODE_CAPTIONS to act(Action.ToggleSubtitles),
            KeyEvent.KEYCODE_S to act(Action.ToggleSubtitles),
            // A game controller works too (as on desktop, M5).
            KeyEvent.KEYCODE_BUTTON_A to KeyCommand.Enter,
            KeyEvent.KEYCODE_BUTTON_B to KeyCommand.Back,
            KeyEvent.KEYCODE_BUTTON_R1 to act(Action.ChannelUp),
            KeyEvent.KEYCODE_BUTTON_L1 to act(Action.ChannelDown),
            KeyEvent.KEYCODE_BUTTON_START to act(Action.TogglePause),
            KeyEvent.KEYCODE_BUTTON_Y to act(Action.ToggleGuide),
            KeyEvent.KEYCODE_BUTTON_X to act(Action.ToggleSubtitles),
            KeyEvent.KEYCODE_BUTTON_R2 to act(Action.NextItem),
            KeyEvent.KEYCODE_BUTTON_L2 to act(Action.PreviousItem),
            KeyEvent.KEYCODE_BUTTON_SELECT to act(Action.ShowInfo),
        )
        for ((key, command) in expected) assertEquals(command, KeyMapping.map(key), KeyEvent.keyCodeToString(key))
        for (d in 0..9) {
            assertEquals(KeyCommand.Digit(d), KeyMapping.map(KeyEvent.KEYCODE_0 + d))
            assertEquals(KeyCommand.Digit(d), KeyMapping.map(KeyEvent.KEYCODE_NUMPAD_0 + d))
        }
        // Keys Marathon doesn't use are left to the system (volume!).
        assertNull(KeyMapping.map(KeyEvent.KEYCODE_VOLUME_UP))
        assertNull(KeyMapping.map(KeyEvent.KEYCODE_HOME))
    }

    /** KeyInput with a fake clock and scheduler. */
    private class Harness {
        val sent = mutableListOf<Action>()
        var now = 1_000L
        val scheduled = mutableListOf<Pair<Long, () -> Unit>>()
        val input = KeyInput(
            send = { sent.add(it) },
            schedule = { delay, block ->
                val entry = (now + delay) to block
                scheduled.add(entry)
                ({ scheduled.remove(entry) })
            },
            nowMs = { now },
        )

        fun press(key: Int, repeat: Int = 0) = input.onKeyDown(key, repeat)

        fun runDue() {
            scheduled.filter { it.first <= now }.forEach {
                scheduled.remove(it)
                it.second()
            }
        }
    }

    @Test
    fun digits_commit_on_ok_or_after_the_timeout() {
        val h = Harness()
        h.press(KeyEvent.KEYCODE_1)
        h.press(KeyEvent.KEYCODE_2)
        assertEquals("12", h.input.pendingDigits())
        h.press(KeyEvent.KEYCODE_DPAD_CENTER)
        assertEquals(listOf<Action>(Action.GoToChannel(12)), h.sent)
        assertTrue(h.scheduled.isEmpty(), "the timeout was cancelled")

        h.sent.clear()
        h.press(KeyEvent.KEYCODE_4)
        h.now += DIGIT_TIMEOUT_MS - 1
        h.runDue()
        assertEquals(emptyList<Action>(), h.sent)
        h.now += 1
        h.runDue()
        assertEquals(listOf<Action>(Action.GoToChannel(4)), h.sent)
    }

    @Test
    fun ok_with_no_digits_selects_and_other_keys_discard_digits() {
        val h = Harness()
        h.press(KeyEvent.KEYCODE_ENTER)
        assertEquals(listOf<Action>(Action.Select), h.sent)
        h.sent.clear()
        h.press(KeyEvent.KEYCODE_7)
        h.press(KeyEvent.KEYCODE_CHANNEL_UP)
        assertEquals("", h.input.pendingDigits())
        assertEquals(listOf<Action>(Action.ChannelUp), h.sent)
    }

    @Test
    fun only_seeking_repeats_when_held() {
        val h = Harness()
        assertTrue(h.press(KeyEvent.KEYCODE_CHANNEL_UP, repeat = 1), "swallowed, not passed on")
        assertTrue(h.press(KeyEvent.KEYCODE_DPAD_RIGHT, repeat = 3))
        h.press(KeyEvent.KEYCODE_MEDIA_REWIND, repeat = 2)
        assertEquals(listOf(Action.Navigate(Direction.Right), Action.SeekRelative(-10)), h.sent)
    }

    @Test
    fun back_clears_digits_then_closes_the_overlay_then_leaves() {
        val h = Harness()
        assertFalse(h.press(KeyEvent.KEYCODE_BACK), "Back goes through the back dispatcher")
        h.press(KeyEvent.KEYCODE_3)
        assertTrue(h.input.onBack(overlayOpen = true))
        assertEquals("", h.input.pendingDigits())
        assertEquals(emptyList<Action>(), h.sent)
        assertTrue(h.input.onBack(overlayOpen = true))
        assertEquals(listOf<Action>(Action.Back), h.sent)
        assertFalse(h.input.onBack(overlayOpen = false), "nothing open: leave the app")
    }

    @Test
    fun unknown_keys_are_not_used() {
        assertFalse(Harness().press(KeyEvent.KEYCODE_VOLUME_DOWN))
    }

    @Test
    fun touch_gestures() {
        val edges = SystemEdges(top = 40f, bottom = 60f)
        fun swipe(startY: Float, dy: Float) = Touch.swipe(startY, dy, height = 1000f, thresholdPx = 100f, edges = edges)
        assertEquals(Action.ChannelUp, swipe(500f, -200f))
        assertEquals(Action.ChannelDown, swipe(500f, 150f))
        assertNull(swipe(500f, -50f))
        assertEquals(Action.ChannelUp, swipe(940f, -200f), "just above the bottom edge")
        assertEquals(Action.ChannelDown, swipe(40f, 200f), "just below the top edge")
        assertNull(swipe(950f, -400f), "from the bottom edge: going home or to recent apps")
        assertNull(swipe(10f, 400f), "from the top edge: pulling down notifications")
        assertEquals(Action.SeekRelative(-10), Touch.doubleTap(10f, 900f))
        assertEquals(Action.TogglePause, Touch.doubleTap(450f, 900f))
        assertEquals(Action.SeekRelative(10), Touch.doubleTap(890f, 900f))
    }
}
