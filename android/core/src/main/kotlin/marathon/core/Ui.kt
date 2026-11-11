// What the Tuner tells the on-screen overlay (PLAN.md §9 "Overlay UI"):
// when to show the channel banner, what it says, and what the guide lists.
// A port of crates/marathon-client/src/ui.rs.
//
// These are local presentation cues, not the Events that observers
// subscribe to: they carry exactly what a screen needs to draw, including
// state the server never hears about (the guide's highlight).
package marathon.core

import marathon.api.models.ChannelSummary
import marathon.api.models.Item
import marathon.api.models.Now

/** A cue for the overlay. */
sealed interface UiEvent {
    /**
     * A channel switch has begun. Sent before the tuner asks the server for
     * the channel or loads anything, so the banner can appear the instant
     * the viewer acts. `banner.title` is what the channel was last known to
     * be on, and may change in the [Playing] that follows.
     */
    data class Tuning(val banner: Banner) : UiEvent

    /**
     * This is what is playing now: after a tune, a skip, an automatic
     * advance, or a failed switch (which reverts to the old channel).
     * [announce] says whether to show the banner; it is false when the
     * channel just moved on by itself.
     */
    data class Playing(val banner: Banner, val announce: Boolean) : UiEvent

    /** Nothing is playing any more (e.g. too many items failed). */
    data object Stopped : UiEvent

    /** The viewer asked what is on: show the banner. */
    data object ShowInfo : UiEvent

    /**
     * The viewer pressed Back with nothing open that the tuner tracks: hide
     * whatever is showing (the banner).
     */
    data object Hide : UiEvent

    /** The guide opened, its highlight moved, or (null) it closed. */
    data class GuideChanged(val guide: Guide?) : UiEvent
}

/** What the channel banner says. */
data class Banner(
    val channelNumber: Int,
    val channelName: String,
    /** The item, e.g. `S03E14 · The Injury` (see [itemTitle]), if known. */
    val title: String?,
    /** The show, if the item has one and it differs from the channel name. */
    val show: String?,
) {
    companion object {
        fun fromNow(now: Now): Banner = of(now.channelNumber, now.channelName, now.item)

        fun fromSummary(channel: ChannelSummary): Banner = of(channel.number, channel.name, channel.current)

        private fun of(number: Int, name: String, item: Item?): Banner = Banner(
            channelNumber = number,
            channelName = name,
            title = item?.let(::itemTitle),
            show = item?.show?.takeIf { it != name },
        )
    }
}

/** The guide: every channel and what it is on. */
data class Guide(
    val rows: List<GuideRow>,
    /** Index into [rows] of the highlighted channel. */
    val highlighted: Int,
) {
    companion object {
        /** The guide for [channels], marking [current] (a channel ID). */
        fun of(channels: List<ChannelSummary>, highlighted: Int, current: String?): Guide = Guide(
            rows = channels.map { c ->
                GuideRow(
                    number = c.number,
                    name = c.name,
                    on = c.current?.let(::itemTitle),
                    current = c.id == current,
                )
            },
            highlighted = highlighted,
        )
    }
}

/** One channel in the guide. */
data class GuideRow(
    val number: Int,
    val name: String,
    /** What the channel is on (see [itemTitle]); null if it has nothing to play. */
    val on: String?,
    /** Whether this is the channel being watched. */
    val current: Boolean,
)

/**
 * An item's title for the screen: `S03E14 · The Injury`, `E02 · Title`, or
 * just the title. (The show is left to the caller; on a channel named after
 * the show it is redundant.)
 */
fun itemTitle(item: Item): String {
    val s = item.season
    val e = item.episode
    return when {
        s != null && e != null -> "S%02dE%02d · %s".format(s, e, item.title)
        e != null -> "E%02d · %s".format(e, item.title)
        else -> item.title
    }
}
