// TunerLoop: actions from any thread reach the tuner on its own thread,
// start-up keeps retrying while the server is down, and shutdown reports
// progress.
@file:Suppress("FunctionName")

package marathon.core

import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import marathon.api.models.AdvanceRequest
import marathon.api.models.ChannelSummary
import marathon.api.models.HistoryEntry
import marathon.api.models.Item
import marathon.api.models.Now
import marathon.api.models.PreviousRequest
import marathon.api.models.ProgressReport
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class TunerLoopTest {
    /** One channel with one item; unreachable until [up] is set. */
    private class FlakyApi : Api {
        @Volatile var up = false
        val progress = CopyOnWriteArrayList<ProgressReport>()
        private val item = Item(id = "i1", missing = false, path = "a.mkv", sourceId = "s", subtitles = emptyList(), title = "A")
        private val now = Now(
            channelId = "c1", channelName = "One", channelNumber = 1, item = item, positionMs = 0, streamUrl = "http://x/i1",
        )

        private fun check() {
            if (!up) throw ApiError.Transport("connection refused")
        }

        override fun channels(): List<ChannelSummary> {
            check()
            return listOf(ChannelSummary(id = "c1", itemCount = 1, name = "One", number = 1, positionMs = 0, current = item))
        }

        override fun now(channelId: String): Now = now.also { check() }

        override fun progress(channelId: String, report: ProgressReport) {
            check()
            progress.add(report)
        }

        override fun advance(channelId: String, request: AdvanceRequest): Now = now.also { check() }

        override fun previous(channelId: String, request: PreviousRequest): Now = now.also { check() }

        override fun history(limit: Int): List<HistoryEntry> = emptyList<HistoryEntry>().also { check() }
    }

    private class QuietPlayer : Player {
        val loaded = CountDownLatch(1)
        @Volatile var isPausedNow = false

        override fun load(request: LoadRequest) {
            loaded.countDown()
        }

        override fun stop() {}

        override fun setPaused(paused: Boolean) {
            isPausedNow = paused
        }

        override fun isPaused() = isPausedNow

        override fun seekRelative(seconds: Int) {}

        override fun positionMs(): Long = 42_000

        override fun durationMs(): Long = 600_000

        override fun toggleSubtitles() {}
    }

    @Test
    fun keeps_retrying_start_until_the_server_is_up_then_handles_actions() {
        val api = FlakyApi()
        val player = QuietPlayer()
        val errors = CopyOnWriteArrayList<String>()
        val loop = TunerLoop(
            tuner = Tuner(api, player, SystemClock, TunerConfig()),
            startBackoff = Backoff(initialMs = 20, maxMs = 40),
            onError = { errors.add(it) },
        )
        loop.launch()
        Thread.sleep(150)
        assertTrue(errors.size >= 2, "retried: $errors")
        assertTrue(errors.all { it.contains("cannot reach the server") }, "$errors")

        api.up = true
        assertTrue(player.loaded.await(5, TimeUnit.SECONDS), "tuned in once the server was up")
        // The load happens inside the tune, just before the loop marks itself started.
        val started = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (!loop.isStarted() && System.nanoTime() < started) Thread.sleep(5)
        assertTrue(loop.isStarted())

        // Actions from another thread are handled on the loop's.
        Thread { loop.send(Action.Pause, "test") }.apply { start() }.join()
        val until = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (!player.isPausedNow && System.nanoTime() < until) Thread.sleep(5)
        assertTrue(player.isPausedNow)

        // Shutting down reports where playback was.
        api.progress.clear()
        loop.shutdown()
        assertEquals(42_000L, api.progress.last().positionMs)
    }
}
