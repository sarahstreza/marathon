// The Tuner: the client's state machine for watching channels (PLAN.md
// §9). A line-for-line port of crates/marathon-client/src/tuner.rs; read
// the two side by side when changing either, and keep TunerTest in step
// with the Rust tests (scripts/check-tuner-parity.sh enforces it).
//
//   Idle → Tuning(channel) → Playing(channel, item) ─ended→ Advancing → Playing
//                                 │
//                                 └─user switches channel→ (report progress) → Tuning(other)
//
// The tuner is synchronous and player-agnostic. It is driven by four
// inputs, all from one thread:
//
// - start once, to tune the first channel;
// - handleAction for actions from the action bus;
// - handlePlayerEvent when the player starts, finishes, or fails an item;
// - tick by nextDeadline, for periodic progress reports.
//
// Tuning and Advancing are transient: they happen inside those calls,
// which block on the Api. Between calls the tuner is Idle or Playing.
//
// Semantics implemented here:
//
// - PLAN.md §1: the resume rewind, the finished threshold on tune-in, and
//   progress reporting (every interval while playing, and immediately on
//   pause, channel change, and quit).
// - PLAN.md §9: context-dependent actions (Navigate, Select, Back) are
//   resolved by Navigation; what happens is published as Events.
// - PLAN.md §9 "Overlay UI": what the screen should show is published as
//   UiEvents. A channel switch announces itself before any network call,
//   so the banner can appear instantly.
// - Standby (PLAN.md M7; on Android, the app going to the background):
//   Standby pauses and reports progress, and Wake resumes, but only if
//   standby was what paused it. Anything that starts playback (play, tune,
//   skip) also ends standby.
// - "Keeps going": an item that fails to load or play is skipped with an
//   Event.Error. After maxConsecutiveFailures failures in a row the tuner
//   stops retrying and says so, rather than looping forever.
//
// Errors: calls throw ApiError when the server can't be reached or
// refuses, and TunerError for the tuner's own failures. The tuner stays
// usable after any of these.
package marathon.core

import marathon.api.models.AdvanceReason
import marathon.api.models.AdvanceRequest
import marathon.api.models.ChannelSummary
import marathon.api.models.ErrorCode
import marathon.api.models.Item
import marathon.api.models.Now
import marathon.api.models.PreviousRequest
import marathon.api.models.ProgressReport

/** Tuning knobs. The defaults are the values from PLAN.md §12. Durations are milliseconds. */
data class TunerConfig(
    /** How far before the saved position to resume. */
    val resumeRewindMs: Long = 5_000,
    /** How often to report progress while playing. */
    val progressIntervalMs: Long = 5_000,
    /**
     * An item is finished when the saved position is within
     * `max(finishedMinMs, finishedFraction × duration)` of its end.
     */
    val finishedMinMs: Long = 30_000,
    val finishedFraction: Double = 0.02,
    /** How many items in a row may fail before the tuner stops skipping ahead and waits for the viewer. */
    val maxConsecutiveFailures: Int = 5,
    /** This client's ID, used to find the channel it watched last. */
    val clientId: String? = null,
) {
    /**
     * Whether a saved [positionMs] counts as finished (PLAN.md §1): within
     * `max(30s, 2% of duration)` of the end. An item with no known duration
     * is never finished, and neither is one at position 0, which keeps
     * clips shorter than the threshold from being skipped forever.
     */
    fun isFinished(positionMs: Long, durationMs: Long?): Boolean {
        val duration = durationMs?.takeIf { it > 0 } ?: return false
        if (positionMs == 0L) return false
        val fraction = (duration.toDouble() * finishedFraction).toLong()
        return positionMs + maxOf(finishedMinMs, fraction) >= duration
    }

    /** Where to start playing an item saved at [positionMs]. */
    fun resumeAt(positionMs: Long): Long = maxOf(0, positionMs - resumeRewindMs)
}

/** A source of the current time in milliseconds, so tests can control it. Only differences matter. */
fun interface Clock {
    fun nowMs(): Long
}

/** The real clock: monotonic, not wall time. */
object SystemClock : Clock {
    override fun nowMs(): Long = System.nanoTime() / 1_000_000
}

/** The tuner's own failures (the Api's are thrown as [ApiError]). */
sealed class TunerError(message: String) : Exception(message) {
    class NoChannels : TunerError("the server has no channels with anything to play")

    class NoSuchChannel(val name: String) : TunerError("there is no channel $name")
}

/** What the caller should do after an action. */
enum class Flow {
    Continue,

    /** The viewer quit; progress has been reported. Shut down. */
    Quit,
}

/** What the tuner is doing between calls. */
sealed interface State {
    /** Nothing tuned yet, or the last tune failed with nothing playing. */
    data object Idle : State

    /** A channel being watched. */
    class Playing internal constructor(
        /** The server's answer for the channel; `now.item` is what the player has loaded (or failed to). */
        val now: Now,
        /**
         * The saved position this item was resumed from, while playback is
         * still inside the resume rewind (see [Tuner.reportProgress]).
         */
        internal var resumeFloor: Long?,
        internal var lastReportMs: Long,
    ) : State
}

/** The Tuner state machine. See the file comment. */
class Tuner<P : Player>(
    private val api: Api,
    val player: P,
    private val clock: Clock,
    private val config: TunerConfig,
    /** Receives the tuner's [Event]s. */
    private val events: (Event) -> Unit = { Log.debug("event $it") },
    /** Receives the tuner's overlay cues. */
    private val ui: (UiEvent) -> Unit = { Log.debug("ui $it") },
) {
    /** The channel list as last fetched, by number. */
    var channels: List<ChannelSummary> = emptyList()
        private set

    var state: State = State.Idle
        private set

    /** What is open on screen (the guide), for the overlay. */
    val navigation = Navigation()

    /** Items in a row that failed to load or play. */
    private var failures = 0

    /**
     * In standby: whether waking should resume playback (it was playing
     * when standby began). Null when not in standby.
     */
    private var standby: Boolean? = null

    /** What is playing, if anything. */
    fun now(): Now? = (state as? State.Playing)?.now

    /** Whether the client is in standby. */
    fun inStandby(): Boolean = standby != null

    private fun emit(event: Event) = events(event)

    private fun cue(cue: UiEvent) = ui(cue)

    /**
     * Tells the overlay what is on now (e.g. after a switch failed, so the
     * banner does not keep naming a channel that never tuned in).
     */
    private fun cueCurrent(announce: Boolean) {
        val now = now()
        cue(if (now != null) UiEvent.Playing(Banner.fromNow(now), announce) else UiEvent.Stopped)
    }

    /** Tells the overlay the guide's state: its rows and highlight, or that it is closed. */
    private fun cueGuide() {
        val guide = navigation.highlighted()?.let { Guide.of(channels, it, now()?.channelId) }
        cue(UiEvent.GuideChanged(guide))
    }

    /**
     * Fetches the channel list and tunes the first channel: channel number
     * [channel] if given, otherwise the one this client watched last (from
     * the server's history), otherwise the lowest-numbered channel with
     * something to play.
     */
    fun start(channel: Int? = null) {
        refreshChannels()
        val target = if (channel != null) {
            channels.find { it.number == channel } ?: throw TunerError.NoSuchChannel(channel.toString())
        } else {
            lastWatched() ?: channels.find { it.itemCount > 0 } ?: throw TunerError.NoChannels()
        }
        tune(target.id)
    }

    /**
     * The channel this client was last tuned to, per the server's history.
     * Falls back to the most recent channel of any client.
     */
    private fun lastWatched(): ChannelSummary? {
        val events = try {
            api.history(200)
        } catch (e: ApiError) {
            Log.warn("cannot read history: ${e.message}")
            return null
        }
        fun watchable(id: String?): ChannelSummary? = id?.let { wanted -> channels.find { it.id == wanted && it.itemCount > 0 } }
        val mine = events.find { e ->
            e.clientId != null && e.clientId == config.clientId && watchable(e.channelId) != null
        }
        val chosen = mine ?: events.find { watchable(it.channelId) != null }
        return chosen?.let { watchable(it.channelId) }
    }

    /**
     * Re-fetches the channel list.
     *
     * Hidden channels (PLAN.md M11) are left out, so channel up/down,
     * numbers, and the guide never land on them; the server leaves them
     * out too, unless asked.
     */
    fun refreshChannels() {
        channels = api.channels().filter { it.hidden != true }.sortedBy { it.number }
    }

    /**
     * Handles one action from the bus. Actions this client does not support
     * are logged and ignored.
     */
    fun handleAction(action: Action): Flow {
        when (action) {
            Action.ChannelUp -> stepChannel(1)
            Action.ChannelDown -> stepChannel(-1)
            is Action.GoToChannel -> goToChannel({ it.number == action.number }, action.number.toString())
            is Action.GoToChannelId -> goToChannelId(action.id)
            Action.TogglePause -> setPaused(!player.isPaused())
            Action.Play -> setPaused(false)
            Action.Pause -> setPaused(true)
            is Action.SeekRelative -> {
                val playing = state as? State.Playing
                if (playing != null) {
                    // An explicit seek is where the viewer wants to be, even
                    // if it is behind the resume point.
                    playing.resumeFloor = null
                    player.seekRelative(action.seconds)
                }
            }
            Action.NextItem -> skip(Skip.Next)
            Action.PreviousItem -> skip(Skip.Previous)
            is Action.Navigate -> when (val resolved = navigation.navigate(action.direction, channels)) {
                is Resolved.Do -> return handleAction(resolved.action)
                Resolved.GuideMoved -> cueGuide()
                Resolved.Nothing -> {}
            }
            Action.Select -> {
                val guideWasOpen = navigation.guideOpen()
                val resolved = navigation.select(channels)
                if (guideWasOpen) {
                    // Selecting closes the guide, before any tuning.
                    cueGuide()
                }
                if (resolved is Resolved.Do) return handleAction(resolved.action)
            }
            Action.Back -> {
                if (navigation.close()) {
                    Log.debug("guide closed")
                    cueGuide()
                } else {
                    cue(UiEvent.Hide)
                }
            }
            Action.ShowInfo -> {
                logInfo()
                cue(UiEvent.ShowInfo)
            }
            Action.ToggleGuide -> {
                if (navigation.close()) {
                    Log.debug("guide closed")
                } else {
                    try {
                        refreshChannels()
                    } catch (e: ApiError) {
                        Log.warn("cannot refresh channels: ${e.message}")
                    }
                    navigation.openGuide(channels, now()?.channelId)
                    Log.debug("guide opened")
                }
                cueGuide()
            }
            Action.ToggleSubtitles -> player.toggleSubtitles()
            // Window business, handled (or ignored) by the app.
            Action.ToggleFullscreen -> {}
            Action.Standby -> enterStandby()
            Action.Wake -> wake()
            Action.Quit -> {
                reportProgress()
                player.stop()
                return Flow.Quit
            }
        }
        return Flow.Continue
    }

    /** Handles a report from the player. */
    fun handlePlayerEvent(event: PlayerEvent) {
        when (event) {
            PlayerEvent.Started -> failures = 0
            PlayerEvent.Ended -> advanceOnEnd()
            is PlayerEvent.Failed -> afterFailure(event.reason)?.let { play(it, announce = false) }
        }
    }

    /** Reports progress if the interval has passed. Call by [nextDeadline]. */
    fun tick() {
        val deadline = nextDeadline() ?: return
        if (clock.nowMs() >= deadline) reportProgress()
    }

    /**
     * When [tick] next has work to do (in [Clock] milliseconds): the next
     * periodic progress report, while something is playing and not paused.
     */
    fun nextDeadline(): Long? {
        val playing = state as? State.Playing ?: return null
        if (player.isPaused()) return null
        return playing.lastReportMs + config.progressIntervalMs
    }

    /**
     * Tells the server where playback is. Called on the interval, on pause,
     * before switching channels, and on quit. Failures are logged, not
     * thrown: a missed report only loses a few seconds.
     *
     * While playback is still inside the resume rewind (behind the position
     * it was resumed from), the resume position is reported instead, so
     * flipping through channels does not walk each one back by the rewind
     * every time.
     */
    fun reportProgress() {
        val playing = state as? State.Playing ?: return
        playing.lastReportMs = clock.nowMs()
        // Still opening the stream: nothing new to report, and reporting 0
        // would lose the saved position.
        var positionMs = player.positionMs() ?: return
        val floor = playing.resumeFloor
        if (floor != null) {
            if (positionMs < floor) positionMs = floor else playing.resumeFloor = null
        }
        val report = ProgressReport(
            itemId = playing.now.item.id,
            positionMs = positionMs,
            durationMs = player.durationMs(),
            paused = player.isPaused(),
        )
        try {
            api.progress(playing.now.channelId, report)
        } catch (e: ApiError) {
            if (e.code() == ErrorCode.STALE_ITEM) {
                // Another client moved the channel on. Keep watching; when
                // this item ends, `advance` (idempotent) catches us up.
                Log.info("channel ${playing.now.channelId} moved on elsewhere")
            } else {
                Log.warn("cannot report progress: ${e.message}")
            }
        }
    }

    /**
     * Goes quiet (the app went to the background, or the TV switched
     * away): pauses, reports progress, and hides the overlay. Remembers
     * whether it was playing, so [wake] resumes only what standby paused.
     */
    private fun enterStandby() {
        if (standby != null) return
        val wasPlaying = state is State.Playing && !player.isPaused()
        Log.info("standby (was playing: $wasPlaying)")
        if (wasPlaying) setPaused(true) else reportProgress()
        standby = wasPlaying
        emit(Event.Standby)
        if (navigation.close()) cueGuide()
        cue(UiEvent.Hide)
    }

    /** Comes back from standby, resuming playback if standby paused it. */
    private fun wake() {
        val resume = standby ?: return
        standby = null
        Log.info("wake (resume: $resume)")
        emit(Event.Wake)
        if (resume) setPaused(false)
    }

    /** Something started playback while in standby (play, tune, skip): standby is over. */
    private fun leaveStandby() {
        if (standby != null) {
            standby = null
            Log.info("playback resumed; leaving standby")
            emit(Event.Wake)
        }
    }

    private fun setPaused(paused: Boolean) {
        if (state !is State.Playing || player.isPaused() == paused) return
        if (!paused) leaveStandby()
        val playing = state as? State.Playing ?: return
        val channelId = playing.now.channelId
        val itemId = playing.now.item.id
        player.setPaused(paused)
        Log.info(if (paused) "paused" else "resumed")
        reportProgress()
        val positionMs = player.positionMs() ?: 0
        emit(
            if (paused) {
                Event.Paused(channelId, itemId, positionMs)
            } else {
                Event.Resumed(channelId, itemId, positionMs)
            },
        )
    }

    /**
     * Tunes to [channelId], reporting progress on the current channel
     * first. If the saved position counts as finished, advances instead of
     * resuming. On failure to reach the server, whatever was playing keeps
     * playing.
     */
    private fun tune(channelId: String) {
        // Announce the switch before anything that waits on the network.
        channels.find { it.id == channelId }?.let { cue(UiEvent.Tuning(Banner.fromSummary(it))) }
        reportProgress()
        try {
            tuneInner(channelId)
        } catch (e: Exception) {
            cueCurrent(announce = true)
            throw e
        }
    }

    private fun tuneInner(channelId: String) {
        var now = api.now(channelId)
        if (config.isFinished(now.positionMs, now.item.durationMs)) {
            Log.info("channel ${now.channelNumber}: saved position of ${now.item.title} is at the end; advancing")
            now = api.advance(channelId, AdvanceRequest(fromItemId = now.item.id, reason = AdvanceReason.ENDED))
        }
        Log.info(
            "tuned in to ${now.channelNumber} ${now.channelName}: ${describe(now)} at ${formatMs(now.positionMs)}",
        )
        emit(Event.ChannelChanged(now.channelId, now.channelNumber, now.channelName, resolutionIds(now)))
        // A fresh choice by the viewer gets a fresh set of retries.
        failures = 0
        play(now, announce = true)
    }

    /**
     * Loads [first] into the player, resuming with the rewind. If the
     * player cannot load it, skips ahead (see [afterFailure]). [announce]
     * says whether the overlay should show the banner for it.
     */
    private fun play(first: Now, announce: Boolean) {
        var now = first
        while (true) {
            reportWarnings(now)
            val startMs = config.resumeAt(now.positionMs)
            val failure = try {
                player.load(loadRequest(now, startMs))
                null
            } catch (e: PlayerError) {
                e
            }
            val event = Event.NowPlaying(
                channelId = now.channelId,
                channelNumber = now.channelNumber,
                itemId = now.item.id,
                title = describe(now),
                positionMs = startMs,
                resolutionPath = resolutionIds(now),
            )
            state = State.Playing(
                now = now,
                resumeFloor = now.positionMs.takeIf { it > startMs },
                lastReportMs = clock.nowMs(),
            )
            if (failure == null) {
                // A load starts playback, which ends standby.
                leaveStandby()
                emit(event)
                cueCurrent(announce)
                return
            }
            now = afterFailure(failure.toString()) ?: return
        }
    }

    /**
     * The current item failed to load or play: report it and skip ahead
     * ("keeps going"). Returns the item to try next, or null once too many
     * items in a row have failed, in which case playback stops and the
     * viewer is told.
     */
    private fun afterFailure(reason: String): Now? {
        val playing = state as? State.Playing ?: return null
        val now = playing.now
        val message = "cannot play ${now.item.path} on channel ${now.channelNumber}: $reason"
        failures += 1
        Log.warn("skipping ($failures in a row): $message")
        emit(Event.Error(message))

        if (failures >= config.maxConsecutiveFailures) {
            val stopped = "channel ${now.channelNumber}: $failures items in a row failed to play; stopped trying. " +
                "Change channel or skip to try again"
            Log.error(stopped)
            emit(Event.Error(stopped))
            player.stop()
            cue(UiEvent.Stopped)
            return null
        }
        val next = api.advance(now.channelId, AdvanceRequest(fromItemId = now.item.id, reason = AdvanceReason.SKIPPED))
        Log.info("skipped ahead on channel ${next.channelNumber} to ${describe(next)}")
        return next
    }

    /**
     * The player finished the current item: advance the channel. The
     * prefetched next item is loaded before asking the server, so the gap
     * between items is only the time to open the stream.
     */
    private fun advanceOnEnd() {
        val playing = state as? State.Playing ?: return
        val channelId = playing.now.channelId
        val fromItemId = playing.now.item.id
        val prefetch = playing.now.next?.let { next ->
            playing.now.copy(
                item = next.item,
                positionMs = 0,
                streamUrl = next.streamUrl,
                audioUrl = next.audioUrl,
                next = null,
                warnings = null,
            )
        }
        emit(Event.ItemEnded(channelId, fromItemId))
        // It played, so the failure streak is over.
        failures = 0
        val preloaded = prefetch?.let { next ->
            try {
                player.load(loadRequest(next, 0))
                next
            } catch (e: PlayerError) {
                Log.warn("cannot preload the next item: $e")
                null
            }
        }
        val now = try {
            api.advance(channelId, AdvanceRequest(fromItemId = fromItemId, reason = AdvanceReason.ENDED))
        } catch (e: ApiError) {
            // The next item is already playing. Track it, so progress
            // reports name it (the server rejects them as stale instead of
            // saving them against the finished item); the next `advance`
            // resynchronizes.
            if (preloaded != null) {
                state = State.Playing(now = preloaded, resumeFloor = null, lastReportMs = clock.nowMs())
            }
            throw e
        }
        Log.info("advanced channel ${now.channelNumber} ${now.channelName} to ${describe(now)}")
        // The server's answer wins. It differs from the prefetch only if
        // another client moved the channel meanwhile, in which case it may
        // also have a saved position worth resuming.
        if (preloaded?.item?.id == now.item.id && now.positionMs <= config.resumeRewindMs) {
            reportWarnings(now)
            emit(
                Event.NowPlaying(
                    channelId = now.channelId,
                    channelNumber = now.channelNumber,
                    itemId = now.item.id,
                    title = describe(now),
                    positionMs = 0,
                    resolutionPath = resolutionIds(now),
                ),
            )
            state = State.Playing(now = now, resumeFloor = null, lastReportMs = clock.nowMs())
            cueCurrent(announce = false)
        } else {
            play(now, announce = false)
        }
    }

    private enum class Skip { Next, Previous }

    /** Skips forward or back within the current channel. */
    private fun skip(direction: Skip) {
        val playing = state as? State.Playing ?: return
        val channelId = playing.now.channelId
        val fromItemId = playing.now.item.id
        val now = when (direction) {
            Skip.Next -> api.advance(channelId, AdvanceRequest(fromItemId = fromItemId, reason = AdvanceReason.SKIPPED))
            Skip.Previous -> api.previous(channelId, PreviousRequest(fromItemId = fromItemId))
        }
        Log.info("skipped ${direction.name.lowercase()} on channel ${now.channelNumber} to ${describe(now)}")
        failures = 0
        play(now, announce = true)
    }

    /**
     * Tunes [step] channels up or down from the current one, skipping
     * channels with nothing to play and wrapping around.
     */
    private fun stepChannel(step: Int) {
        if (channels.isEmpty()) refreshChannels()
        val watchable = channels.filter { it.itemCount > 0 }
        if (watchable.isEmpty()) throw TunerError.NoChannels()
        val current = now()?.channelNumber
        val target = when {
            current == null -> watchable.first()
            step > 0 -> watchable.find { it.number > current } ?: watchable.first()
            else -> watchable.lastOrNull { it.number < current } ?: watchable.last()
        }
        tune(target.id)
    }

    /**
     * Tunes to the channel matching [wanted], refreshing the channel list
     * once if none does. [name] describes the channel for errors.
     */
    private fun goToChannel(wanted: (ChannelSummary) -> Boolean, name: String) {
        val id = channels.find(wanted)?.id ?: run {
            // Channels may have been added to the config since.
            refreshChannels()
            channels.find(wanted)?.id ?: throw TunerError.NoSuchChannel(name)
        }
        if (now()?.channelId == id) return
        tune(id)
    }

    /**
     * Tunes to a channel by ID. Unlike by number, this reaches hidden
     * channels too, which are not in the channel list: the server is asked
     * directly, and says if there is no such channel.
     */
    private fun goToChannelId(id: String) {
        if (now()?.channelId == id) return
        try {
            tune(id)
        } catch (e: ApiError) {
            if (e.code() == ErrorCode.NOT_FOUND) throw TunerError.NoSuchChannel(id)
            throw e
        }
    }

    /**
     * Reports what the server stepped past to find [now]'s item (such as a
     * child channel with nothing to play, PLAN.md M11) as Error events. The
     * server already caps how many it steps past in one go.
     */
    private fun reportWarnings(now: Now) {
        for (message in now.warnings.orEmpty()) {
            Log.warn("channel ${now.channelNumber}: the server skipped something: $message")
            emit(Event.Error(message))
        }
    }

    private fun logInfo() {
        val now = now()
        if (now == null) {
            Log.info("nothing playing")
            return
        }
        Log.info(
            "now playing on ${now.channelNumber} ${now.channelName}: ${describe(now)} " +
                "at ${player.positionMs()?.let(::formatMs).orEmpty()} of ${player.durationMs()?.let(::formatMs).orEmpty()}" +
                if (player.isPaused()) " (paused)" else "",
        )
    }
}

private fun subtitleTracks(item: Item): List<SubtitleTrack> =
    item.subtitles.map { SubtitleTrack(url = it.url, title = it.label, language = it.language) }

/** What to tell the player to play for [now], from [startMs]. */
private fun loadRequest(now: Now, startMs: Long): LoadRequest = LoadRequest(
    url = now.streamUrl,
    startMs = startMs,
    subtitles = subtitleTracks(now.item),
    audioUrl = now.audioUrl,
    durationMs = now.item.durationMs,
)

/** A one-line description of the current item, e.g. `Show · S03E14 · The Injury`. */
/**
 * The IDs of the channels [now]'s item was resolved through (PLAN.md M11),
 * for events. A server older than nested channels sends no path; the
 * channel itself is the whole path then.
 */
private fun resolutionIds(now: Now): List<String> =
    now.resolutionPath?.takeIf { it.isNotEmpty() }?.map { it.id } ?: listOf(now.channelId)

fun describe(now: Now): String {
    val item = now.item
    val parts = mutableListOf<String>()
    item.show?.let(parts::add)
    val s = item.season
    val e = item.episode
    when {
        s != null && e != null -> parts.add("S%02dE%02d".format(s, e))
        e != null -> parts.add("E%02d".format(e))
    }
    parts.add(item.title)
    return parts.joinToString(" · ")
}

/** Formats milliseconds as `h:mm:ss` or `m:ss`. */
fun formatMs(ms: Long): String {
    val s = ms / 1000
    val h = s / 3600
    val m = (s / 60) % 60
    val sec = s % 60
    return if (h > 0) "%d:%02d:%02d".format(h, m, sec) else "%d:%02d".format(m, sec)
}
