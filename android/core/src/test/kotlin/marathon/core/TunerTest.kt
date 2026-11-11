// Tuner scenarios against an in-memory fake server and a mock player. A
// port of crates/marathon-client/src/tuner/tests.rs: every test there has
// a twin here with the same name (scripts/check-tuner-parity.sh checks),
// so the Kotlin Tuner passes the same scenarios as the Rust one (M10).
// Both fakes record into one shared journal, so tests can assert the order
// of API calls and player commands.
@file:Suppress("ktlint:standard:function-naming", "FunctionName", "TestFunctionName")

package marathon.core

import marathon.api.models.AdvanceReason
import marathon.api.models.AdvanceRequest
import marathon.api.models.ChannelRef
import marathon.api.models.ChannelSummary
import marathon.api.models.ErrorCode
import marathon.api.models.EventKind
import marathon.api.models.HistoryEntry
import marathon.api.models.Item
import marathon.api.models.NextItem
import marathon.api.models.Now
import marathon.api.models.PreviousRequest
import marathon.api.models.ProgressReport
import marathon.api.models.Subtitle
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

private const val MIN = 60_000L

/** One API call or player command, in the order it happened. */
private sealed interface Entry {
    data object Channels : Entry

    data class NowCall(val channel: String) : Entry

    data class Progress(val channel: String, val report: ProgressReport) : Entry

    data class Advance(val channel: String, val from: String, val reason: AdvanceReason) : Entry

    data class Previous(val channel: String, val from: String) : Entry

    data object History : Entry

    data class Load(val request: LoadRequest) : Entry

    data object Stop : Entry

    data class Pause(val paused: Boolean) : Entry

    data class Seek(val seconds: Int) : Entry

    data object ToggleSubtitles : Entry
}

private fun item(id: String, durationMs: Long?) = Item(
    id = id,
    missing = false,
    path = "$id.mkv",
    sourceId = "lib",
    subtitles = emptyList(),
    title = id,
    durationMs = durationMs,
)

private fun url(id: String) = "http://server/api/v1/stream/$id"

private class FakeChannel(val id: String, val number: Int, var items: MutableList<Item>) {
    var current = 0
    var positionMs = 0L

    /** Hidden channels are left out of `channels`, as the server does. */
    var hidden = false

    /** Child channels the items come through (PLAN.md M11). */
    var via: List<ChannelRef> = emptyList()

    /** What `now` and `advance` report the server skipped. */
    var warnings: List<String> = emptyList()

    fun now(): Now {
        val item = items[current]
        val next = items[(current + 1) % items.size]
        return Now(
            channelId = id,
            channelName = "Channel $number",
            channelNumber = number,
            item = item,
            positionMs = positionMs,
            streamUrl = url(item.id),
            next = NextItem(item = next, streamUrl = url(next.id)),
            resolutionPath = listOf(ChannelRef(id = id, name = "Channel $number")) + via,
            warnings = warnings,
        )
    }

    fun currentId(): String = items[current].id
}

/** Mimics the server: ordered channels that loop, idempotent advance and previous, and stale progress rejected with 409. */
private class FakeServer(val channels: MutableList<FakeChannel>) {
    var history: List<HistoryEntry> = emptyList()

    /** Makes `advance` fail as if the server were unreachable. */
    var failAdvance = false

    fun channel(id: String): FakeChannel =
        channels.find { it.id == id } ?: throw ApiError.Server(404, ErrorCode.NOT_FOUND, "no channel $id")
}

private class FakeApi(val server: FakeServer, val journal: MutableList<Entry>) : Api {
    /** API calls so far. */
    var calls = 0

    private fun log(entry: Entry) {
        calls += 1
        journal.add(entry)
    }

    override fun channels(): List<ChannelSummary> {
        log(Entry.Channels)
        return server.channels.filter { !it.hidden }.map { c ->
            ChannelSummary(
                id = c.id,
                itemCount = c.items.size,
                name = "Channel ${c.number}",
                number = c.number,
                positionMs = c.positionMs,
                current = c.items.getOrNull(c.current),
            )
        }
    }

    override fun now(channelId: String): Now {
        log(Entry.NowCall(channelId))
        val c = server.channel(channelId)
        if (c.items.isEmpty()) throw ApiError.Server(409, ErrorCode.CHANNEL_EMPTY, "empty")
        return c.now()
    }

    override fun progress(channelId: String, report: ProgressReport) {
        log(Entry.Progress(channelId, report))
        val c = server.channel(channelId)
        if (c.currentId() != report.itemId) throw ApiError.Server(409, ErrorCode.STALE_ITEM, "stale")
        c.positionMs = report.positionMs
    }

    override fun advance(channelId: String, request: AdvanceRequest): Now {
        log(Entry.Advance(channelId, request.fromItemId, request.reason))
        if (server.failAdvance) throw ApiError.Transport("connection refused")
        val c = server.channel(channelId)
        if (c.currentId() == request.fromItemId) {
            c.current = (c.current + 1) % c.items.size
            c.positionMs = 0
        }
        return c.now()
    }

    override fun previous(channelId: String, request: PreviousRequest): Now {
        log(Entry.Previous(channelId, request.fromItemId))
        val c = server.channel(channelId)
        if (c.currentId() == request.fromItemId && c.current > 0) {
            c.current -= 1
            c.positionMs = 0
        }
        return c.now()
    }

    override fun history(limit: Int): List<HistoryEntry> {
        log(Entry.History)
        return server.history.take(limit)
    }
}

/** Records commands; position, duration, and pause state are set by tests. */
private class MockPlayer(val journal: MutableList<Entry>) : Player {
    /** Item IDs whose loads fail. */
    val broken = mutableSetOf<String>()
    var position: Long? = null
    var duration: Long? = 20 * MIN
    private var paused = false

    override fun load(request: LoadRequest) {
        journal.add(Entry.Load(request))
        if (broken.any { request.url == url(it) }) {
            position = null
            throw PlayerError("no such file")
        }
        paused = false
        position = request.startMs
    }

    override fun stop() {
        journal.add(Entry.Stop)
    }

    override fun setPaused(paused: Boolean) {
        journal.add(Entry.Pause(paused))
        this.paused = paused
    }

    override fun isPaused(): Boolean = paused

    override fun seekRelative(seconds: Int) {
        journal.add(Entry.Seek(seconds))
        position?.let { position = maxOf(0, it + seconds * 1000L) }
    }

    override fun positionMs(): Long? = position

    override fun durationMs(): Long? = duration

    override fun toggleSubtitles() {
        journal.add(Entry.ToggleSubtitles)
    }
}

/** A tuner over the fake server, plus handles to steer and inspect it. */
private class Harness(channels: Int, items: Int) {
    val journal = mutableListOf<Entry>()
    val server = FakeServer(
        (1..channels).map { n ->
            FakeChannel("c$n", n, (0 until items).map { i -> item("c$n-$i", 20 * MIN) }.toMutableList())
        }.toMutableList(),
    )
    val api = FakeApi(server, journal)
    val player = MockPlayer(journal)
    var clockMs = 1_000_000L
    val events = mutableListOf<Event>()

    /** UI cues, each with the number of API calls made before it. */
    val ui = mutableListOf<Pair<Int, UiEvent>>()
    val tuner = Tuner(
        api = api,
        player = player,
        clock = { clockMs },
        config = TunerConfig(clientId = "me"),
        events = { events.add(it) },
        ui = { ui.add(api.calls to it) },
    )

    /** Takes the UI cues so far. */
    fun takeUi(): List<UiEvent> = ui.map { it.second }.also { ui.clear() }

    fun setSaved(channel: String, current: Int, positionMs: Long) {
        val c = server.channel(channel)
        c.current = current
        c.positionMs = positionMs
    }

    fun saved(channel: String): Pair<String, Long> = server.channel(channel).let { it.currentId() to it.positionMs }

    fun advanceClock(byMs: Long) {
        clockMs += byMs
    }

    /** Takes the events emitted so far. */
    fun takeEvents(): List<Event> = events.toList().also { events.clear() }

    /** Takes the journal so far. */
    fun take(): List<Entry> = journal.toList().also { journal.clear() }

    fun action(action: Action): Flow = tuner.handleAction(action)

    fun playingItem(): String = tuner.now()!!.item.id

    var position: Long?
        get() = player.position
        set(value) {
            player.position = value
        }
}

private fun progressReports(entries: List<Entry>): List<Pair<String, ProgressReport>> =
    entries.filterIsInstance<Entry.Progress>().map { it.channel to it.report }

private fun load(id: String, startMs: Long): Entry = Entry.Load(LoadRequest(url = url(id), startMs = startMs, subtitles = emptyList()))

private fun errors(events: List<Event>): List<String> = events.filterIsInstance<Event.Error>().map { it.message }

private fun advances(entries: List<Entry>, reason: AdvanceReason): Int =
    entries.count { it is Entry.Advance && it.reason == reason }

private fun banner(number: Int, title: String) = Banner(channelNumber = number, channelName = "Channel $number", title = title, show = null)

class TunerTest {
    @Test
    fun tune_plays_at_saved_position_minus_rewind() {
        val h = Harness(2, 3)
        h.setSaved("c1", 1, 10 * MIN)
        h.tuner.start(1)
        val entries = h.take()
        assertTrue(entries.contains(Entry.NowCall("c1")))
        assertEquals(load("c1-1", 10 * MIN - 5_000), entries.last())
        assertEquals("c1-1", h.playingItem())
    }

    @Test
    fun rewind_never_goes_before_the_start() {
        val h = Harness(1, 2)
        h.setSaved("c1", 0, 3_000)
        h.tuner.start(1)
        assertEquals(load("c1-0", 0), h.take().last())
    }

    @Test
    fun tune_in_within_finished_threshold_advances_instead_of_resuming() {
        val h = Harness(1, 3)
        // 20-minute item: threshold is max(30s, 24s) = 30s. 20s left.
        h.setSaved("c1", 0, 20 * MIN - 20_000)
        h.tuner.start(1)
        val entries = h.take()
        assertTrue(entries.contains(Entry.Advance("c1", "c1-0", AdvanceReason.ENDED)), "$entries")
        assertEquals(load("c1-1", 0), entries.last())
        assertEquals("c1-1" to 0L, h.saved("c1"))
    }

    @Test
    fun tune_in_just_outside_finished_threshold_resumes() {
        val h = Harness(1, 3)
        h.setSaved("c1", 0, 20 * MIN - 31_000)
        h.tuner.start(1)
        assertEquals(load("c1-0", 20 * MIN - 36_000), h.take().last())
    }

    @Test
    fun finished_threshold_is_the_larger_of_30s_and_2_percent() {
        val config = TunerConfig()
        val hour = 60 * MIN
        // 2% of an hour is 72s.
        assertTrue(config.isFinished(hour - 72_000, hour))
        assertFalse(config.isFinished(hour - 73_000, hour))
        // For 10 minutes 2% is 12s, so 30s applies.
        assertTrue(config.isFinished(10 * MIN - 30_000, 10 * MIN))
        assertFalse(config.isFinished(10 * MIN - 31_000, 10 * MIN))
        // Past the end counts too.
        assertTrue(config.isFinished(hour + 1, hour))
        // Unknown duration, or not started: never finished, even for clips
        // shorter than the threshold.
        assertFalse(config.isFinished(hour, null))
        assertFalse(config.isFinished(0, 6_000))
        assertTrue(config.isFinished(1, 6_000))
    }

    @Test
    fun end_of_file_advances_and_plays_the_next_item() {
        val h = Harness(1, 3)
        h.tuner.start(1)
        h.take()
        h.tuner.handlePlayerEvent(PlayerEvent.Ended)
        // The prefetched next item is loaded first, then the server is told.
        assertEquals(listOf(load("c1-1", 0), Entry.Advance("c1", "c1-0", AdvanceReason.ENDED)), h.take())
        assertEquals("c1-1", h.playingItem())
        assertEquals("c1-1", h.saved("c1").first)

        // And again, using the new `next`.
        h.tuner.handlePlayerEvent(PlayerEvent.Ended)
        assertEquals(load("c1-2", 0), h.take()[0])
        assertEquals("c1-2", h.playingItem())
    }

    @Test
    fun end_of_file_follows_the_server_if_another_client_moved_the_channel() {
        val h = Harness(1, 4)
        h.tuner.start(1)
        // Another client advanced twice and watched a minute of item 2.
        h.setSaved("c1", 2, MIN)
        h.take()
        h.tuner.handlePlayerEvent(PlayerEvent.Ended)
        val entries = h.take()
        assertEquals(load("c1-1", 0), entries[0]) // the stale prefetch
        assertEquals(load("c1-2", MIN - 5_000), entries.last())
        assertEquals("c1-2", h.playingItem())
    }

    @Test
    fun failed_advance_at_end_does_not_save_progress_against_the_old_item() {
        val h = Harness(1, 3)
        h.setSaved("c1", 0, 19 * MIN)
        h.tuner.start(1)
        h.server.failAdvance = true
        assertThrows<ApiError.Transport> { h.tuner.handlePlayerEvent(PlayerEvent.Ended) }
        // The prefetched item plays and is what the tuner tracks...
        assertEquals("c1-1", h.playingItem())
        // ...so its progress is rejected as stale rather than overwriting the
        // finished item's position.
        h.position = MIN
        h.action(Action.Quit)
        assertEquals("c1-0" to 19 * MIN, h.saved("c1"))

        // Once the server is back, the next end resynchronizes.
        h.server.failAdvance = false
        h.tuner.handlePlayerEvent(PlayerEvent.Ended)
        assertEquals("c1-0", h.playingItem())
    }

    @Test
    fun channel_switch_reports_progress_before_tuning_away() {
        val h = Harness(3, 2)
        h.tuner.start(1)
        h.position = 7 * MIN
        h.take()

        assertEquals(Flow.Continue, h.action(Action.ChannelUp))
        val entries = h.take()
        val report = Entry.Progress(
            "c1",
            ProgressReport(itemId = "c1-0", positionMs = 7 * MIN, durationMs = 20 * MIN, paused = false),
        )
        val reportAt = entries.indexOf(report)
        val nowAt = entries.indexOf(Entry.NowCall("c2"))
        assertTrue(reportAt >= 0 && reportAt < nowAt, "$entries")
        assertEquals(load("c2-0", 0), entries.last())
        assertEquals("c1-0" to 7 * MIN, h.saved("c1"))

        // Coming back resumes where we left off, minus the rewind.
        h.action(Action.ChannelDown)
        assertEquals(load("c1-0", 7 * MIN - 5_000), h.take().last())
    }

    @Test
    fun progress_is_reported_on_the_interval() {
        val h = Harness(1, 2)
        h.tuner.start(1)
        h.take()
        val interval = 5_000L
        assertEquals(h.clockMs + interval, h.tuner.nextDeadline(), "first report is due one interval after tuning")

        h.advanceClock(interval - 1)
        h.position = 4_999
        h.tuner.tick()
        assertEquals(emptyList<Entry>(), h.take(), "not due yet")

        h.advanceClock(1)
        h.position = 5_000
        h.tuner.tick()
        var reports = progressReports(h.take())
        assertEquals(1, reports.size)
        assertEquals(5_000L, reports[0].second.positionMs)
        assertEquals(false, reports[0].second.paused)
        assertEquals(h.clockMs + interval, h.tuner.nextDeadline())

        h.advanceClock(interval)
        h.position = 10_000
        h.tuner.tick()
        reports = progressReports(h.take())
        assertEquals(1, reports.size)
        assertEquals(10_000L, h.saved("c1").second)
    }

    @Test
    fun progress_is_reported_immediately_on_pause_and_unpause() {
        val h = Harness(1, 2)
        h.tuner.start(1)
        h.position = MIN
        h.take()

        h.action(Action.TogglePause)
        var entries = h.take()
        assertEquals(Entry.Pause(true), entries[0])
        var reports = progressReports(entries)
        assertEquals(1, reports.size)
        assertEquals(true, reports[0].second.paused)
        assertEquals(MIN, reports[0].second.positionMs)

        // No periodic reports while paused.
        assertNull(h.tuner.nextDeadline())
        h.advanceClock(60_000)
        h.tuner.tick()
        assertEquals(emptyList<Entry>(), h.take())

        h.action(Action.TogglePause)
        entries = h.take()
        assertEquals(Entry.Pause(false), entries[0])
        reports = progressReports(entries)
        assertEquals(1, reports.size)
        assertEquals(false, reports[0].second.paused)
    }

    @Test
    fun progress_is_reported_on_quit() {
        val h = Harness(1, 2)
        h.tuner.start(1)
        h.position = 3 * MIN
        h.take()
        assertEquals(Flow.Quit, h.action(Action.Quit))
        val entries = h.take()
        val reports = progressReports(entries)
        assertEquals(1, reports.size)
        assertEquals(3 * MIN, reports[0].second.positionMs)
        assertEquals(Entry.Stop, entries.last())
        assertEquals(3 * MIN, h.saved("c1").second)
    }

    @Test
    fun quick_flips_do_not_walk_the_position_back() {
        val h = Harness(2, 2)
        h.setSaved("c1", 0, 10 * MIN)
        h.tuner.start(1)
        // Two seconds after resuming 5s early: still behind the saved spot.
        h.position = 10 * MIN - 3_000
        h.action(Action.ChannelUp)
        assertEquals(10 * MIN, h.saved("c1").second)

        // Once playback passes the saved position, the real position counts.
        h.action(Action.ChannelDown)
        h.position = 10 * MIN + 2_000
        h.action(Action.ChannelUp)
        assertEquals(10 * MIN + 2_000, h.saved("c1").second)
    }

    @Test
    fun seeking_back_is_respected_even_inside_the_rewind() {
        val h = Harness(1, 2)
        h.setSaved("c1", 0, 10 * MIN)
        h.tuner.start(1)
        h.action(Action.SeekRelative(-10))
        h.action(Action.Quit)
        assertEquals(10 * MIN - 15_000, h.saved("c1").second)
    }

    @Test
    fun position_unknown_while_loading_is_not_reported_as_zero() {
        val h = Harness(2, 2)
        h.setSaved("c1", 0, 10 * MIN)
        h.tuner.start(1)
        h.position = null
        h.action(Action.ChannelUp)
        assertEquals(10 * MIN, h.saved("c1").second)
    }

    @Test
    fun next_and_previous_items() {
        val h = Harness(1, 3)
        h.tuner.start(1)
        h.take()
        h.action(Action.NextItem)
        var entries = h.take()
        assertEquals(Entry.Advance("c1", "c1-0", AdvanceReason.SKIPPED), entries[0])
        assertEquals(load("c1-1", 0), entries.last())

        h.action(Action.PreviousItem)
        entries = h.take()
        assertEquals(Entry.Previous("c1", "c1-1"), entries[0])
        assertEquals(load("c1-0", 0), entries.last())
        assertEquals("c1-0", h.playingItem())
    }

    @Test
    fun stale_progress_is_ignored_and_end_catches_up() {
        val h = Harness(1, 4)
        h.tuner.start(1)
        // Another client moves the channel to item 2.
        h.setSaved("c1", 2, 0)
        h.position = MIN
        h.advanceClock(5_000)
        h.tuner.tick() // rejected as stale; must not error or stop playback
        assertEquals("c1-0", h.playingItem())
        assertEquals("c1-2" to 0L, h.saved("c1"))
        h.tuner.handlePlayerEvent(PlayerEvent.Ended)
        assertEquals("c1-2", h.playingItem())
    }

    @Test
    fun channel_up_and_down_wrap_and_skip_empty_channels() {
        val h = Harness(4, 2)
        h.server.channels[2].items.clear() // channel 3 is empty
        h.tuner.start(1)
        val seen = mutableListOf<Int>()
        repeat(4) {
            h.action(Action.ChannelUp)
            seen.add(h.tuner.now()!!.channelNumber)
        }
        assertEquals(listOf(2, 4, 1, 2), seen)
        seen.clear()
        repeat(3) {
            h.action(Action.ChannelDown)
            seen.add(h.tuner.now()!!.channelNumber)
        }
        assertEquals(listOf(1, 4, 2), seen)
    }

    @Test
    fun go_to_channel_by_number() {
        val h = Harness(3, 2)
        h.tuner.start(1)
        h.action(Action.GoToChannel(3))
        assertEquals("c3", h.tuner.now()!!.channelId)
        h.take()
        // Same channel: nothing happens.
        h.action(Action.GoToChannel(3))
        assertEquals(emptyList<Entry>(), h.take())
        // Unknown channel: refreshes the list, then reports the error.
        val err = assertThrows<TunerError.NoSuchChannel> { h.tuner.handleAction(Action.GoToChannel(9)) }
        assertEquals("9", err.name)
        assertEquals(listOf<Entry>(Entry.Channels), h.take())
        assertEquals("c3", h.tuner.now()!!.channelId)
    }

    @Test
    fun start_resumes_the_channel_this_client_watched_last() {
        fun event(client: String, channel: String) = HistoryEntry(
            id = "e",
            kind = EventKind.PROGRESS,
            tsMs = 0,
            channelId = channel,
            clientId = client,
        )
        // Newest first: someone else on c3, then us on c2.
        var h = Harness(3, 2)
        h.server.history = listOf(event("other", "c3"), event("me", "c2"))
        h.tuner.start(null)
        assertEquals("c2", h.tuner.now()!!.channelId)

        // With no history of our own, the most recent channel of anyone.
        h = Harness(3, 2)
        h.server.history = listOf(event("other", "c3"))
        h.tuner.start(null)
        assertEquals("c3", h.tuner.now()!!.channelId)

        // With no history at all, the first channel.
        h = Harness(3, 2)
        h.tuner.start(null)
        assertEquals("c1", h.tuner.now()!!.channelId)
    }

    @Test
    fun failed_tune_keeps_the_current_channel_playing() {
        val h = Harness(2, 2)
        h.tuner.start(1)
        h.server.channels[1].items.clear()
        // The cached list still says channel 2 has items; the server says no.
        val err = assertThrows<ApiError.Server> { h.tuner.handleAction(Action.ChannelUp) }
        assertEquals("server returned 409: empty", err.message)
        assertEquals("c1", h.tuner.now()!!.channelId)
    }

    @Test
    fun subtitles_are_passed_to_the_player() {
        val h = Harness(1, 2)
        val c = h.server.channels[0]
        c.items[0] = c.items[0].copy(
            subtitles = listOf(
                Subtitle(format = "srt", index = 0, label = "en", url = "http://server/api/v1/subtitles/c1-0/0", language = "en"),
            ),
        )
        h.tuner.start(1)
        val request = (h.take().last() as Entry.Load).request
        assertEquals(
            listOf(SubtitleTrack(url = "http://server/api/v1/subtitles/c1-0/0", title = "en", language = "en")),
            request.subtitles,
        )
        h.action(Action.ToggleSubtitles)
        assertEquals(listOf<Entry>(Entry.ToggleSubtitles), h.take())
    }

    @Test
    fun describe_and_format() {
        val h = Harness(1, 1)
        h.tuner.start(1)
        val now = h.tuner.now()!!.let {
            it.copy(item = it.item.copy(show = "The Office", season = 3, episode = 14, title = "The Injury"))
        }
        assertEquals("The Office · S03E14 · The Injury", describe(now))
        assertEquals("1:05", formatMs(65_000))
        assertEquals("1:02:05", formatMs(3_725_000))
    }

    @Test
    fun an_item_that_fails_to_load_is_skipped_with_an_error_event() {
        val h = Harness(1, 4)
        h.player.broken.add("c1-0")
        h.tuner.start(1)
        val entries = h.take()
        assertTrue(entries.contains(Entry.Advance("c1", "c1-0", AdvanceReason.SKIPPED)))
        assertEquals(load("c1-1", 0), entries.last())
        assertEquals("c1-1", h.playingItem())
        val events = h.takeEvents()
        val errs = errors(events)
        assertEquals(1, errs.size)
        assertTrue(errs[0].contains("c1-0.mkv"), errs[0])
        assertEquals("c1-1", (events.last() as Event.NowPlaying).itemId)
    }

    @Test
    fun an_item_that_fails_while_playing_is_skipped() {
        val h = Harness(1, 4)
        h.tuner.start(1)
        h.take()
        h.takeEvents()
        // e.g. the file was deleted: mpv opens the stream, then gets a 404.
        h.tuner.handlePlayerEvent(PlayerEvent.Failed("HTTP 404"))
        val entries = h.take()
        assertEquals(1, advances(entries, AdvanceReason.SKIPPED))
        assertEquals("c1-1", h.playingItem())
        assertEquals(1, errors(h.takeEvents()).size)
    }

    @Test
    fun stops_retrying_after_too_many_failures_in_a_row() {
        val h = Harness(1, 8)
        for (i in 0 until 8) h.player.broken.add("c1-$i")
        h.tuner.start(1) // does not loop forever, does not throw
        val entries = h.take()
        assertEquals(5, entries.count { it is Entry.Load }, "default maxConsecutiveFailures")
        assertEquals(4, advances(entries, AdvanceReason.SKIPPED))
        assertEquals(Entry.Stop, entries.last())
        val errs = errors(h.takeEvents())
        assertEquals(6, errs.size, "$errs")
        assertTrue(errs[5].contains("stopped trying"), errs[5])

        // Nothing else happens on its own...
        h.advanceClock(60_000)
        h.tuner.tick()
        assertTrue(h.take().none { it is Entry.Load })
        // ...but the viewer can try again, with a fresh set of retries.
        h.player.broken.clear()
        h.action(Action.NextItem)
        assertTrue(h.take().last() is Entry.Load)
    }

    @Test
    fun a_started_item_ends_the_failure_streak() {
        val h = Harness(1, 10)
        val configMax = TunerConfig().maxConsecutiveFailures
        // Four bad items, a good one, then four more bad: never gives up.
        for (i in listOf(0, 1, 2, 3, 5, 6, 7, 8)) h.player.broken.add("c1-$i")
        h.tuner.start(1)
        assertEquals("c1-4", h.playingItem())
        h.tuner.handlePlayerEvent(PlayerEvent.Started)
        h.tuner.handlePlayerEvent(PlayerEvent.Ended)
        assertEquals("c1-9", h.playingItem())
        val errs = errors(h.takeEvents())
        assertEquals(8, errs.size)
        assertTrue(errs.none { it.contains("stopped trying") })
        assertEquals(5, configMax)
    }

    @Test
    fun window_actions_are_ignored() {
        val h = Harness(1, 2)
        h.tuner.start(1)
        h.take()
        assertEquals(Flow.Continue, h.action(Action.ToggleFullscreen))
        assertEquals(emptyList<Entry>(), h.take())
        assertEquals("c1-0", h.playingItem())
    }

    @Test
    fun standby_pauses_and_reports_and_wake_resumes() {
        val h = Harness(1, 2)
        h.tuner.start(1)
        h.position = 3 * MIN
        h.take()
        h.takeEvents()

        h.action(Action.Standby)
        assertTrue(h.tuner.inStandby())
        val entries = h.take()
        assertEquals(Entry.Pause(true), entries[0])
        val reports = progressReports(entries)
        assertEquals(1, reports.size)
        assertEquals(3 * MIN, reports[0].second.positionMs)
        assertEquals(true, reports[0].second.paused)
        assertEquals("c1-0" to 3 * MIN, h.saved("c1"))
        var events = h.takeEvents()
        assertTrue(events[0] is Event.Paused, "$events")
        assertEquals(Event.Standby, events[1])
        // Paused: no periodic reports while in standby.
        assertNull(h.tuner.nextDeadline())

        // Standby again changes nothing.
        h.action(Action.Standby)
        assertEquals(emptyList<Entry>(), h.take())
        assertEquals(emptyList<Event>(), h.takeEvents())

        h.action(Action.Wake)
        assertFalse(h.tuner.inStandby())
        assertEquals(Entry.Pause(false), h.take()[0])
        events = h.takeEvents()
        assertEquals(Event.Wake, events[0])
        assertTrue(events[1] is Event.Resumed, "$events")
        // Waking when awake changes nothing.
        h.action(Action.Wake)
        assertEquals(emptyList<Entry>(), h.take())
        assertEquals(emptyList<Event>(), h.takeEvents())
    }

    @Test
    fun wake_keeps_a_pause_the_viewer_chose() {
        val h = Harness(1, 2)
        h.tuner.start(1)
        h.position = MIN
        h.action(Action.Pause)
        h.take()
        h.takeEvents()

        h.action(Action.Standby)
        // Already paused: just a progress report.
        val entries = h.take()
        assertFalse(entries.contains(Entry.Pause(true)), "$entries")
        assertEquals(1, progressReports(entries).size)
        h.action(Action.Wake)
        assertEquals(emptyList<Entry>(), h.take())
        assertTrue(h.tuner.player.isPaused())
        assertEquals(listOf(Event.Standby, Event.Wake), h.takeEvents())
    }

    @Test
    fun playing_anything_ends_standby() {
        val h = Harness(2, 2)
        h.tuner.start(1)
        h.position = MIN

        // Pressing play.
        h.action(Action.Standby)
        h.takeEvents()
        h.action(Action.TogglePause)
        assertFalse(h.tuner.inStandby())
        var events = h.takeEvents()
        assertEquals(Event.Wake, events[0], "$events")
        assertTrue(events[1] is Event.Resumed)

        // Changing channel.
        h.action(Action.Standby)
        h.takeEvents()
        h.action(Action.ChannelUp)
        assertFalse(h.tuner.inStandby())
        assertFalse(h.tuner.player.isPaused())
        events = h.takeEvents()
        assertTrue(events.contains(Event.Wake), "$events")

        // Skipping.
        h.action(Action.Standby)
        h.takeEvents()
        h.action(Action.NextItem)
        assertFalse(h.tuner.inStandby())
        assertTrue(h.takeEvents().contains(Event.Wake))

        // A later wake has nothing to do.
        h.take()
        h.action(Action.Wake)
        assertEquals(emptyList<Entry>(), h.take())
        assertEquals(emptyList<Event>(), h.takeEvents())
    }

    @Test
    fun standby_hides_the_overlay_and_closes_the_guide() {
        val h = Harness(2, 2)
        h.tuner.start(1)
        h.action(Action.ToggleGuide)
        h.takeUi()
        h.action(Action.Standby)
        assertEquals(listOf(UiEvent.GuideChanged(null), UiEvent.Hide), h.takeUi())
        assertFalse(h.tuner.navigation.guideOpen())
    }

    @Test
    fun standby_with_nothing_playing_does_not_panic() {
        val h = Harness(1, 2)
        h.action(Action.Standby)
        assertTrue(h.tuner.inStandby())
        h.action(Action.Wake)
        assertFalse(h.tuner.inStandby())
        assertEquals(listOf(Event.Standby, Event.Wake), h.takeEvents())
    }

    @Test
    fun actions_before_anything_plays_do_not_panic() {
        val h = Harness(2, 2)
        for (
        action in listOf(
            Action.TogglePause,
            Action.Play,
            Action.Pause,
            Action.SeekRelative(10),
            Action.NextItem,
            Action.PreviousItem,
            Action.Select,
            Action.Back,
            Action.ShowInfo,
            Action.ToggleSubtitles,
            Action.Navigate(Direction.Left),
        )
        ) {
            h.tuner.handleAction(action)
        }
        assertEquals(State.Idle, h.tuner.state)
    }

    @Test
    fun navigate_without_the_guide_changes_channel_and_seeks() {
        val h = Harness(3, 2)
        h.tuner.start(1)
        h.take()
        h.action(Action.Navigate(Direction.Up))
        assertEquals(2, h.tuner.now()!!.channelNumber)
        h.action(Action.Navigate(Direction.Down))
        assertEquals(1, h.tuner.now()!!.channelNumber)
        h.take()
        h.action(Action.Navigate(Direction.Right))
        h.action(Action.Navigate(Direction.Left))
        assertEquals(listOf<Entry>(Entry.Seek(10), Entry.Seek(-10)), h.take())
    }

    @Test
    fun navigate_with_the_guide_moves_the_selection_and_select_tunes() {
        val h = Harness(3, 2)
        h.tuner.start(1)
        h.action(Action.ToggleGuide)
        assertTrue(h.tuner.navigation.guideOpen())
        h.take()

        h.action(Action.Navigate(Direction.Down))
        h.action(Action.Navigate(Direction.Down))
        h.action(Action.Navigate(Direction.Left))
        // Moving the highlight neither tunes nor seeks.
        assertEquals(emptyList<Entry>(), h.take())
        assertEquals(1, h.tuner.now()!!.channelNumber)
        assertEquals(2, h.tuner.navigation.highlighted())

        h.action(Action.Select)
        assertEquals("c3", h.tuner.now()!!.channelId)
        assertFalse(h.tuner.navigation.guideOpen())

        // Back closes the guide; arrows change channel again.
        h.action(Action.ToggleGuide)
        h.action(Action.Back)
        h.action(Action.Navigate(Direction.Up))
        assertEquals(1, h.tuner.now()!!.channelNumber)
    }

    @Test
    fun select_with_nothing_open_shows_info() {
        val h = Harness(1, 2)
        h.tuner.start(1)
        h.take()
        h.action(Action.Select)
        assertEquals(emptyList<Entry>(), h.take(), "info is logged, nothing else")
    }

    @Test
    fun go_to_channel_by_id() {
        val h = Harness(3, 2)
        h.tuner.start(1)
        h.action(Action.GoToChannelId("c2"))
        assertEquals(2, h.tuner.now()!!.channelNumber)
        val err = assertThrows<TunerError.NoSuchChannel> { h.tuner.handleAction(Action.GoToChannelId("nope")) }
        assertEquals("nope", err.name)
    }

    @Test
    fun hidden_channels_are_skipped_by_flipping_and_number_but_reachable_by_id() {
        val h = Harness(3, 2)
        h.server.channels[1].hidden = true
        h.tuner.start(1)
        h.action(Action.ChannelUp)
        assertEquals("c3", h.tuner.now()!!.channelId)
        h.action(Action.ChannelDown)
        assertEquals("c1", h.tuner.now()!!.channelId)
        val err = assertThrows<TunerError.NoSuchChannel> { h.tuner.handleAction(Action.GoToChannel(2)) }
        assertEquals("2", err.name)
        assertEquals("c1", h.tuner.now()!!.channelId)

        // By ID, the hidden channel is reachable.
        h.action(Action.GoToChannelId("c2"))
        assertEquals("c2", h.tuner.now()!!.channelId)
        // Flipping from it goes to its visible neighbours.
        h.action(Action.ChannelUp)
        assertEquals("c3", h.tuner.now()!!.channelId)
        // And the guide lists only visible channels.
        h.action(Action.ToggleGuide)
        val guide = h.takeUi().filterIsInstance<UiEvent.GuideChanged>().last().guide!!
        assertEquals(listOf(1, 3), guide.rows.map { it.number })
    }

    @Test
    fun nested_channels_report_their_resolution_path() {
        val h = Harness(2, 2)
        h.server.channels[0].via = listOf(ChannelRef(id = "child", name = "Child"))
        h.tuner.start(1)
        val events = h.takeEvents()
        val path = listOf("c1", "child")
        assertTrue(events.any { it is Event.ChannelChanged && it.resolutionPath == path }, "$events")
        assertTrue(events.any { it is Event.NowPlaying && it.resolutionPath == path }, "$events")
        val banner = h.takeUi().filterIsInstance<UiEvent.Playing>().first().banner
        assertEquals("Child: c1-0", banner.itemLine())
    }

    @Test
    fun what_the_server_skipped_is_reported_as_errors() {
        val h = Harness(2, 2)
        val warning = "channel \"empty\" has nothing to play; channel \"c1\" skipped it"
        h.server.channels[0].warnings = listOf(warning)
        h.tuner.start(1)
        val events = h.takeEvents()
        val error = events.indexOfFirst { it is Event.Error && it.message == warning }
        val playing = events.indexOfFirst { it is Event.NowPlaying }
        assertTrue(error >= 0 && error < playing, "$events")
        // Playback carries on.
        assertEquals("c1-0", h.tuner.now()!!.item.id)

        // Advancing at the end of an item reports them too.
        h.tuner.handlePlayerEvent(PlayerEvent.Ended)
        assertTrue(h.takeEvents().any { it is Event.Error && it.message == warning })
    }

    @Test
    fun play_and_pause_are_idempotent_and_emit_events() {
        val h = Harness(1, 2)
        h.tuner.start(1)
        h.position = MIN
        h.take()
        h.takeEvents()

        h.action(Action.Play) // already playing
        assertEquals(emptyList<Entry>(), h.take())
        h.action(Action.Pause)
        h.action(Action.Pause)
        val entries = h.take()
        assertEquals(Entry.Pause(true), entries[0])
        assertEquals(1, progressReports(entries).size)
        h.action(Action.Play)
        assertEquals(Entry.Pause(false), h.take()[0])
        val events = h.takeEvents()
        assertEquals(2, events.size, "$events")
        assertEquals(MIN, (events[0] as Event.Paused).positionMs)
        assertTrue(events[1] is Event.Resumed)
    }

    @Test
    fun tuning_and_advancing_emit_events() {
        val h = Harness(2, 3)
        h.tuner.start(1)
        var events = h.takeEvents()
        assertEquals(2, events.size, "$events")
        assertEquals("c1", (events[0] as Event.ChannelChanged).channelId)
        assertEquals("c1-0", (events[1] as Event.NowPlaying).itemId)
        h.tuner.handlePlayerEvent(PlayerEvent.Ended)
        events = h.takeEvents()
        assertEquals(2, events.size, "$events")
        assertEquals("c1-0", (events[0] as Event.ItemEnded).itemId)
        assertEquals("c1-1", (events[1] as Event.NowPlaying).itemId)
    }

    @Test
    fun the_banner_is_cued_before_the_switch_touches_the_network() {
        val h = Harness(3, 2)
        h.setSaved("c2", 1, MIN)
        h.tuner.start(1)
        h.takeUi()
        h.take()
        val callsBefore = h.api.calls

        h.action(Action.ChannelUp)
        val cues = h.ui.toList()
        // The first cue names the target channel and what it was last on,
        // and came before any API call for the switch (progress, now).
        assertEquals(callsBefore to UiEvent.Tuning(banner(2, "c2-1")), cues[0])
        assertTrue(h.api.calls > callsBefore)
        assertEquals(UiEvent.Playing(banner(2, "c2-1"), announce = true), cues[1].second)
        assertEquals(2, cues.size, "$cues")
    }

    @Test
    fun a_failed_switch_cues_the_channel_still_playing() {
        val h = Harness(2, 2)
        h.tuner.start(1)
        h.server.channels[1].items.clear()
        h.takeUi()
        assertThrows<ApiError> { h.tuner.handleAction(Action.ChannelUp) }
        assertEquals(
            listOf(UiEvent.Tuning(banner(2, "c2-0")), UiEvent.Playing(banner(1, "c1-0"), announce = true)),
            h.takeUi(),
        )
    }

    @Test
    fun automatic_advances_update_the_banner_without_announcing() {
        val h = Harness(1, 3)
        h.tuner.start(1)
        h.takeUi()
        h.tuner.handlePlayerEvent(PlayerEvent.Ended)
        assertEquals(listOf(UiEvent.Playing(banner(1, "c1-1"), announce = false)), h.takeUi())
        // A skip is the viewer's doing, so it is announced.
        h.action(Action.NextItem)
        assertEquals(listOf(UiEvent.Playing(banner(1, "c1-2"), announce = true)), h.takeUi())
    }

    @Test
    fun info_back_and_the_guide_are_cued() {
        val h = Harness(3, 2)
        h.tuner.start(2)
        h.takeUi()

        h.action(Action.ShowInfo)
        h.action(Action.Back)
        assertEquals(listOf(UiEvent.ShowInfo, UiEvent.Hide), h.takeUi())
        h.action(Action.Select)
        assertEquals(listOf<UiEvent>(UiEvent.ShowInfo), h.takeUi())

        fun row(n: Int, current: Boolean) = GuideRow(number = n, name = "Channel $n", on = "c$n-0", current = current)
        val rows = listOf(row(1, false), row(2, true), row(3, false))
        fun guide(highlighted: Int) = UiEvent.GuideChanged(Guide(rows, highlighted))
        h.action(Action.ToggleGuide)
        h.action(Action.Navigate(Direction.Down))
        h.action(Action.Back)
        assertEquals(listOf(guide(1), guide(2), UiEvent.GuideChanged(null)), h.takeUi())

        // Selecting closes the guide before the switch is announced.
        h.action(Action.ToggleGuide)
        h.action(Action.Navigate(Direction.Up))
        h.takeUi()
        h.action(Action.Select)
        val cues = h.takeUi()
        assertEquals(UiEvent.GuideChanged(null), cues[0])
        assertEquals(UiEvent.Tuning(banner(1, "c1-0")), cues[1])
    }

    @Test
    fun giving_up_after_failures_cues_stopped() {
        val h = Harness(1, 2)
        h.player.broken.addAll(listOf("c1-0", "c1-1"))
        h.tuner.start(1)
        assertEquals(UiEvent.Stopped, h.takeUi().last())
    }
}
