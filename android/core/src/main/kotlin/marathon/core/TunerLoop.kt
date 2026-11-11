// The thread that owns the Tuner. The Tuner is synchronous and must only
// be touched from one thread (as on desktop, where it has its own thread);
// everything else (key presses, touch, the remote control channel, mpv's
// callbacks) hands it work through this loop's queue, from any thread.
//
// The loop also keeps the client going: if the server can't be reached at
// start, it keeps retrying with Backoff, reporting each failure through
// onError, rather than giving up.
package marathon.core

import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Runs [tuner] on its own thread. Call [launch] once; [send] and
 * [playerEvent] from any thread; [shutdown] to quit (reporting progress).
 *
 * @param startChannel the channel number to start on, or null for the last one watched.
 * @param onError called (on the loop's thread) with a message for the viewer when a call fails.
 * @param onQuit called (on the loop's thread) after a [Action.Quit] has been handled.
 */
class TunerLoop<P : Player>(
    private val tuner: Tuner<P>,
    private val clock: Clock = SystemClock,
    private val startChannel: Int? = null,
    private val startBackoff: Backoff = Backoff(initialMs = 1_000, maxMs = 30_000),
    private val onError: (String) -> Unit = {},
    private val onQuit: () -> Unit = {},
) {
    private sealed interface Input {
        data class Act(val action: Action, val source: String) : Input

        data class Played(val event: PlayerEvent) : Input

        data object Shutdown : Input
    }

    private val queue = LinkedBlockingQueue<Input>()
    private var thread: Thread? = null

    @Volatile
    private var started = false

    /** Starts the loop's thread, which tunes the first channel. */
    fun launch() {
        check(thread == null) { "already launched" }
        thread = Thread(::run, "tuner").apply {
            isDaemon = true
            start()
        }
    }

    /** Queues an action from the input called [source] (e.g. `keys`, `touch`, `remote`). */
    fun send(action: Action, source: String = "app") {
        queue.put(Input.Act(action, source))
    }

    /** Queues a report from the player. */
    fun playerEvent(event: PlayerEvent) {
        queue.put(Input.Played(event))
    }

    /** Reports progress, stops the player, and ends the loop; waits up to [timeoutMs] for it. */
    fun shutdown(timeoutMs: Long = 3_000) {
        queue.put(Input.Shutdown)
        thread?.join(timeoutMs)
    }

    /** Whether the first channel has tuned in. */
    fun isStarted(): Boolean = started

    private fun run() {
        var nextStartAttempt: Long? = clock.nowMs()
        while (true) {
            if (nextStartAttempt != null && clock.nowMs() >= nextStartAttempt) {
                nextStartAttempt = try {
                    tuner.start(startChannel)
                    started = true
                    startBackoff.reset()
                    null
                } catch (e: Exception) {
                    if (e is InterruptedException) throw e
                    Log.warn("cannot start: ${e.message}")
                    onError(e.message ?: e.toString())
                    clock.nowMs() + startBackoff.nextDelayMs()
                }
            }
            val deadline = listOfNotNull(tuner.nextDeadline(), nextStartAttempt).minOrNull()
            val input = if (deadline == null) {
                queue.take()
            } else {
                queue.poll(maxOf(0, deadline - clock.nowMs()), TimeUnit.MILLISECONDS)
            }
            when (input) {
                null -> tuner.tick()
                Input.Shutdown -> {
                    guard { tuner.handleAction(Action.Quit) }
                    return
                }
                is Input.Act -> {
                    Log.debug("action ${input.action} from ${input.source}")
                    val flow = guard { tuner.handleAction(input.action) }
                    if (flow == Flow.Quit) {
                        onQuit()
                        return
                    }
                }
                is Input.Played -> guard { tuner.handlePlayerEvent(input.event) }
            }
            // Until the first tune works, the viewer may still have tuned in
            // by hand (e.g. typing a channel number): that counts as started.
            if (!started && tuner.now() != null) {
                started = true
                nextStartAttempt = null
            }
        }
    }

    /** Runs [block], reporting failures instead of ending the loop ("keeps going"). */
    private fun <T> guard(block: () -> T): T? = try {
        block()
    } catch (e: InterruptedException) {
        throw e
    } catch (e: Exception) {
        Log.warn("${e.message}")
        onError(e.message ?: e.toString())
        null
    }
}
