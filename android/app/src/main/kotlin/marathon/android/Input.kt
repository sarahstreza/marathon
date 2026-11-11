// Turning remote-control keys and touch gestures into Actions (PLAN.md §9:
// actions are semantic, not device-shaped). The tables are in
// docs/ANDROID.md and docs/CONTROLS.md; KeyMappingTest and TouchTest check
// them.
package marathon.android

import android.view.KeyEvent
import marathon.core.Action
import marathon.core.ChannelNumberEntry
import marathon.core.Direction
import marathon.core.SEEK_STEP_SECS

/** What a key means, before digit entry is resolved. */
sealed interface KeyCommand {
    /** Carry out [action]. [repeats] says whether holding the key repeats it (only seeking does). */
    data class Act(val action: Action, val repeats: Boolean = false) : KeyCommand

    /** A digit for channel-number entry. */
    data class Digit(val digit: Int) : KeyCommand

    /** OK / Enter: commits a pending channel number, otherwise selects. */
    data object Enter : KeyCommand

    /** Back: discards a pending channel number, otherwise goes back. */
    data object Back : KeyCommand
}

object KeyMapping {
    /** What [keyCode] means, or null if Marathon doesn't use the key (so the system can). */
    fun map(keyCode: Int): KeyCommand? = when (keyCode) {
        KeyEvent.KEYCODE_DPAD_UP -> KeyCommand.Act(Action.Navigate(Direction.Up))
        KeyEvent.KEYCODE_DPAD_DOWN -> KeyCommand.Act(Action.Navigate(Direction.Down))
        // Left/Right seek with nothing open, so they repeat like seeking.
        KeyEvent.KEYCODE_DPAD_LEFT -> KeyCommand.Act(Action.Navigate(Direction.Left), repeats = true)
        KeyEvent.KEYCODE_DPAD_RIGHT -> KeyCommand.Act(Action.Navigate(Direction.Right), repeats = true)
        KeyEvent.KEYCODE_DPAD_CENTER, KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER,
        KeyEvent.KEYCODE_BUTTON_A,
        -> KeyCommand.Enter
        KeyEvent.KEYCODE_BACK, KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_BUTTON_B -> KeyCommand.Back
        KeyEvent.KEYCODE_CHANNEL_UP, KeyEvent.KEYCODE_PAGE_UP, KeyEvent.KEYCODE_BUTTON_R1 -> KeyCommand.Act(Action.ChannelUp)
        KeyEvent.KEYCODE_CHANNEL_DOWN, KeyEvent.KEYCODE_PAGE_DOWN, KeyEvent.KEYCODE_BUTTON_L1 -> KeyCommand.Act(Action.ChannelDown)
        KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE, KeyEvent.KEYCODE_SPACE, KeyEvent.KEYCODE_BUTTON_START,
        KeyEvent.KEYCODE_HEADSETHOOK,
        -> KeyCommand.Act(Action.TogglePause)
        KeyEvent.KEYCODE_MEDIA_PLAY -> KeyCommand.Act(Action.Play)
        // Stop pauses: Marathon keeps going, and stopping would lose the channel.
        KeyEvent.KEYCODE_MEDIA_PAUSE, KeyEvent.KEYCODE_MEDIA_STOP -> KeyCommand.Act(Action.Pause)
        KeyEvent.KEYCODE_MEDIA_REWIND, KeyEvent.KEYCODE_MEDIA_SKIP_BACKWARD, KeyEvent.KEYCODE_MEDIA_STEP_BACKWARD ->
            KeyCommand.Act(Action.SeekRelative(-SEEK_STEP_SECS), repeats = true)
        KeyEvent.KEYCODE_MEDIA_FAST_FORWARD, KeyEvent.KEYCODE_MEDIA_SKIP_FORWARD, KeyEvent.KEYCODE_MEDIA_STEP_FORWARD ->
            KeyCommand.Act(Action.SeekRelative(SEEK_STEP_SECS), repeats = true)
        KeyEvent.KEYCODE_MEDIA_NEXT, KeyEvent.KEYCODE_N, KeyEvent.KEYCODE_BUTTON_R2 -> KeyCommand.Act(Action.NextItem)
        KeyEvent.KEYCODE_MEDIA_PREVIOUS, KeyEvent.KEYCODE_P, KeyEvent.KEYCODE_BUTTON_L2 -> KeyCommand.Act(Action.PreviousItem)
        KeyEvent.KEYCODE_INFO, KeyEvent.KEYCODE_I, KeyEvent.KEYCODE_MENU, KeyEvent.KEYCODE_BUTTON_SELECT ->
            KeyCommand.Act(Action.ShowInfo)
        KeyEvent.KEYCODE_GUIDE, KeyEvent.KEYCODE_G, KeyEvent.KEYCODE_BUTTON_Y -> KeyCommand.Act(Action.ToggleGuide)
        KeyEvent.KEYCODE_CAPTIONS, KeyEvent.KEYCODE_S, KeyEvent.KEYCODE_BUTTON_X -> KeyCommand.Act(Action.ToggleSubtitles)
        in KeyEvent.KEYCODE_0..KeyEvent.KEYCODE_9 -> KeyCommand.Digit(keyCode - KeyEvent.KEYCODE_0)
        in KeyEvent.KEYCODE_NUMPAD_0..KeyEvent.KEYCODE_NUMPAD_9 -> KeyCommand.Digit(keyCode - KeyEvent.KEYCODE_NUMPAD_0)
        else -> null
    }
}

/**
 * Key presses with channel-number entry: digits are buffered (shared
 * [ChannelNumberEntry], as on desktop) and committed by OK/Enter or after
 * a pause; any other key discards them.
 *
 * @param send delivers the resulting actions.
 * @param schedule runs a callback after a delay (on the same thread as [onKeyDown]); returns a canceller.
 * @param nowMs the current time in milliseconds.
 */
class KeyInput(
    private val send: (Action) -> Unit,
    private val schedule: (delayMs: Long, block: () -> Unit) -> () -> Unit,
    private val nowMs: () -> Long,
) {
    private val digits = ChannelNumberEntry()
    private var cancelTimeout: (() -> Unit)? = null

    /** The digits typed so far, for display. */
    fun pendingDigits(): String = digits.pending()

    /**
     * Handles a key press; [repeat] is the key's repeat count. Returns
     * whether the key was used. Back is never used here: Android delivers
     * it through the back dispatcher (see [onBack]).
     */
    fun onKeyDown(keyCode: Int, repeat: Int): Boolean {
        val command = KeyMapping.map(keyCode) ?: return false
        if (command == KeyCommand.Back) return false
        if (repeat > 0 && !(command is KeyCommand.Act && command.repeats)) return true
        when (command) {
            is KeyCommand.Digit -> {
                digits.digit(command.digit, nowMs())
                cancelTimeout?.invoke()
                cancelTimeout = digits.deadline()?.let { deadline ->
                    schedule(deadline - nowMs()) { digits.tick(nowMs())?.let(send) }
                }
            }
            KeyCommand.Enter -> {
                val number = commitDigits()
                send(number ?: Action.Select)
            }
            KeyCommand.Back -> return false
            is KeyCommand.Act -> {
                clearDigits()
                send(command.action)
            }
        }
        return true
    }

    /**
     * Back was pressed. Discards a pending channel number; otherwise sends
     * Back if [overlayOpen] (something on screen to close). Returns false
     * when there was nothing to go back from, so the system should (leave
     * the app).
     */
    fun onBack(overlayOpen: Boolean): Boolean {
        if (!digits.isEmpty()) {
            clearDigits()
            return true
        }
        if (!overlayOpen) return false
        send(Action.Back)
        return true
    }

    private fun commitDigits(): Action? {
        cancelTimeout?.invoke()
        cancelTimeout = null
        return digits.commit()
    }

    private fun clearDigits() {
        cancelTimeout?.invoke()
        cancelTimeout = null
        digits.clear()
    }
}

/** Touch gestures (phones), as pure functions of where and how far. */
object Touch {
    /** A vertical swipe of [dy] pixels (up is negative), or null if under [thresholdPx]. */
    fun swipe(dy: Float, thresholdPx: Float): Action? = when {
        dy <= -thresholdPx -> Action.ChannelUp
        dy >= thresholdPx -> Action.ChannelDown
        else -> null
    }

    /** A double tap at [x] on a view [width] wide: left third seeks back, right third forward, middle pauses. */
    fun doubleTap(x: Float, width: Float): Action = when {
        x < width / 3 -> Action.SeekRelative(-SEEK_STEP_SECS)
        x > width * 2 / 3 -> Action.SeekRelative(SEEK_STEP_SECS)
        else -> Action.TogglePause
    }
}
