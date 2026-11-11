// Resolving context-dependent actions (PLAN.md §9): what Navigate, Select,
// and Back mean depends on what is open. A port of
// crates/marathon-client/src/navigation.rs.
//
// - Nothing open: Up/Down change channel, Left/Right seek ∓10s, Select
//   shows info.
// - Guide open: Up/Down move the highlighted channel (wrapping), Left/Right
//   do nothing, Select tunes to the highlighted channel and closes the
//   guide, Back closes it.
package marathon.core

import marathon.api.models.ChannelSummary

/** Seconds that Left and Right seek by with nothing open. */
const val SEEK_STEP_SECS: Int = 10

/** What a navigation action turned out to mean. */
sealed interface Resolved {
    /** Carry out this (context-free) action. */
    data class Do(val action: Action) : Resolved

    /** The guide's highlight moved; redraw it. */
    data object GuideMoved : Resolved

    /** Nothing to do in this context. */
    data object Nothing : Resolved
}

/** What is open on screen, for resolving navigation. */
class Navigation {
    /** Index of the highlighted channel, if the guide is open. */
    private var guide: Int? = null

    fun guideOpen(): Boolean = guide != null

    /** Index of the highlighted channel, if the guide is open. */
    fun highlighted(): Int? = guide

    /** Opens the guide highlighting [current] (a channel ID), or the first channel. */
    fun openGuide(channels: List<ChannelSummary>, current: String?) {
        val index = channels.indexOfFirst { it.id == current }
        guide = if (index >= 0) index else 0
    }

    /** Closes whatever is open. Returns whether anything was. */
    fun close(): Boolean {
        val wasOpen = guide != null
        guide = null
        return wasOpen
    }

    /** Resolves [Action.Navigate]. */
    fun navigate(direction: Direction, channels: List<ChannelSummary>): Resolved {
        val i = guide ?: return when (direction) {
            Direction.Up -> Resolved.Do(Action.ChannelUp)
            Direction.Down -> Resolved.Do(Action.ChannelDown)
            Direction.Left -> Resolved.Do(Action.SeekRelative(-SEEK_STEP_SECS))
            Direction.Right -> Resolved.Do(Action.SeekRelative(SEEK_STEP_SECS))
        }
        if (channels.isEmpty()) return Resolved.Nothing
        val last = channels.size - 1
        return when (direction) {
            // The channel list may have shrunk since the guide opened, so an
            // out-of-range highlight is treated as the last channel.
            Direction.Up -> {
                guide = if (i == 0 || i > last) last else i - 1
                Resolved.GuideMoved
            }
            Direction.Down -> {
                guide = if (i >= last) 0 else i + 1
                Resolved.GuideMoved
            }
            Direction.Left, Direction.Right -> Resolved.Nothing
        }
    }

    /** Resolves [Action.Select]. */
    fun select(channels: List<ChannelSummary>): Resolved {
        val i = guide ?: return Resolved.Do(Action.ShowInfo)
        guide = null
        val channel = channels.getOrNull(i) ?: return Resolved.Nothing
        return Resolved.Do(Action.GoToChannelId(channel.id))
    }
}
