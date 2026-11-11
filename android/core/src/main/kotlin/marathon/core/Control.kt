// The control plane (PLAN.md §9 "Input and control"): Actions that every
// input emits and Events that observers subscribe to. A port of
// crates/marathon-api/src/control.rs.
//
// These are written by hand, not generated: in OpenAPI they are `oneOf`
// with a discriminator, which the Kotlin generator flattens into one class
// with every variant's fields. They must stay wire-compatible with the Rust
// types; ControlJsonTest checks every variant against the fixtures that
// crates/marathon-api/tests/control_fixtures.rs writes.
@file:OptIn(ExperimentalSerializationApi::class)

package marathon.core

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonClassDiscriminator

/** A direction for [Action.Navigate]. */
@Serializable
enum class Direction {
    @SerialName("up") Up,
    @SerialName("down") Down,
    @SerialName("left") Left,
    @SerialName("right") Right,
}

/**
 * Something a viewer (or a program acting for one) asked for. Actions are
 * semantic, not device-shaped: a TV remote's D-pad, a touch swipe, and
 * arrow keys all produce [Navigate] or a channel change, and the [Tuner]
 * decides what they mean in context.
 */
@Serializable
@JsonClassDiscriminator("action")
sealed interface Action {
    /** Tune to the next channel up (higher number, wrapping). */
    @Serializable @SerialName("channel_up") data object ChannelUp : Action

    /** Tune to the next channel down (lower number, wrapping). */
    @Serializable @SerialName("channel_down") data object ChannelDown : Action

    /** Tune to the channel with this number. */
    @Serializable @SerialName("go_to_channel") data class GoToChannel(val number: Int) : Action

    /** Tune to the channel with this ID. */
    @Serializable @SerialName("go_to_channel_id") data class GoToChannelId(val id: String) : Action

    @Serializable @SerialName("toggle_pause") data object TogglePause : Action

    /** Resume playback (no-op if playing). */
    @Serializable @SerialName("play") data object Play : Action

    /** Pause playback (no-op if paused). */
    @Serializable @SerialName("pause") data object Pause : Action

    /** Seek by this many seconds; negative is backwards. */
    @Serializable @SerialName("seek_relative") data class SeekRelative(val seconds: Int) : Action

    /** Skip to the channel's next item. */
    @Serializable @SerialName("next_item") data object NextItem : Action

    /** Step back to the channel's previous item. */
    @Serializable @SerialName("previous_item") data object PreviousItem : Action

    /**
     * Move in a direction. With nothing open, Up/Down change channel and
     * Left/Right seek; with the guide open, they move its selection.
     */
    @Serializable @SerialName("navigate") data class Navigate(val direction: Direction) : Action

    /** Choose the highlighted thing (a channel in the guide), or show info when nothing is open. */
    @Serializable @SerialName("select") data object Select : Action

    /** Close whatever is open. */
    @Serializable @SerialName("back") data object Back : Action

    /** Show what is playing. */
    @Serializable @SerialName("show_info") data object ShowInfo : Action

    /** Open or close the channel guide. */
    @Serializable @SerialName("toggle_guide") data object ToggleGuide : Action

    /** Cycle subtitle tracks (on/off with one track). */
    @Serializable @SerialName("toggle_subtitles") data object ToggleSubtitles : Action

    /** Switch between fullscreen and windowed. Android is always fullscreen. */
    @Serializable @SerialName("toggle_fullscreen") data object ToggleFullscreen : Action

    /**
     * Go quiet (e.g. the app went to the background): pause, save the
     * position, and hide the overlay, ready to wake.
     */
    @Serializable @SerialName("standby") data object Standby : Action

    /** Come back from standby, resuming playback if standby paused it. */
    @Serializable @SerialName("wake") data object Wake : Action

    /** Save the position and exit. */
    @Serializable @SerialName("quit") data object Quit : Action
}

/** Something that happened on this client, for observers (`GET /events`). */
@Serializable
@JsonClassDiscriminator("event")
sealed interface Event {
    /** An item started playing (after a tune, an advance, or a skip). */
    @Serializable
    @SerialName("now_playing")
    data class NowPlaying(
        @SerialName("channel_id") val channelId: String,
        @SerialName("channel_number") val channelNumber: Int,
        @SerialName("item_id") val itemId: String,
        /** Human-readable description, e.g. `Show · S03E14 · The Injury`. */
        val title: String,
        /** Where playback started within the item. */
        @SerialName("position_ms") val positionMs: Long,
        /**
         * The channels the item was resolved through (nested channels,
         * PLAN.md M11): [channelId] first, ending with the channel the item
         * belongs to.
         */
        @SerialName("resolution_path") val resolutionPath: List<String> = emptyList(),
    ) : Event

    /** The client tuned to a different channel. */
    @Serializable
    @SerialName("channel_changed")
    data class ChannelChanged(
        @SerialName("channel_id") val channelId: String,
        @SerialName("channel_number") val channelNumber: Int,
        @SerialName("channel_name") val channelName: String,
        /** As for [NowPlaying]. */
        @SerialName("resolution_path") val resolutionPath: List<String> = emptyList(),
    ) : Event

    @Serializable
    @SerialName("paused")
    data class Paused(
        @SerialName("channel_id") val channelId: String,
        @SerialName("item_id") val itemId: String,
        @SerialName("position_ms") val positionMs: Long,
    ) : Event

    @Serializable
    @SerialName("resumed")
    data class Resumed(
        @SerialName("channel_id") val channelId: String,
        @SerialName("item_id") val itemId: String,
        @SerialName("position_ms") val positionMs: Long,
    ) : Event

    /** An item played to its end. */
    @Serializable
    @SerialName("item_ended")
    data class ItemEnded(
        @SerialName("channel_id") val channelId: String,
        @SerialName("item_id") val itemId: String,
    ) : Event

    @Serializable @SerialName("standby") data object Standby : Event

    @Serializable @SerialName("wake") data object Wake : Event

    /** Something went wrong; playback carries on where it can. */
    @Serializable @SerialName("error") data class Error(val message: String) : Event
}

/** JSON for the control plane, as the server speaks it. */
val ControlJson: Json = Json {
    // A newer server may add fields; an unknown *variant* still fails, so
    // it is logged and dropped rather than misread.
    ignoreUnknownKeys = true
}

fun Action.toJson(): String = ControlJson.encodeToString(Action.serializer(), this)

fun Event.toJson(): String = ControlJson.encodeToString(Event.serializer(), this)
