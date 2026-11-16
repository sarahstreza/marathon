// The decisions behind PlaybackService's media session, browse tree, and
// artwork that don't need Android, kept here so they're unit tested
// (docs/ANDROID.md "Android Auto").
package marathon.android

import kotlin.math.abs
import kotlin.math.roundToInt
import marathon.android.widget.WidgetChannel

object CarMedia {
    /**
     * Apps allowed to browse Marathon's channels through PlaybackService
     * (MediaBrowserService): Android Auto on a phone, and the media centre
     * of Android Automotive OS. Others (System UI's media resumption, for
     * one) get no browse root, so they can't bind Marathon's session.
     *
     * A package name is unique on a device, so another app can't pose as
     * one of these while the real one is installed. Without it, Marathon
     * would only show a list of channel names.
     */
    val CAR_BROWSERS = setOf(
        "com.google.android.projection.gearhead",
        "com.android.car.media",
    )

    fun isCarBrowser(packageName: String): Boolean = packageName in CAR_BROWSERS

    /**
     * The `SeekRelative` seconds that move playback from [currentMs] to
     * [targetMs] (a scrub on a car's or lock screen's seek bar), or null
     * if it's within half a second already, or the position isn't known.
     */
    fun seekSeconds(targetMs: Long, currentMs: Long?): Int? {
        if (currentMs == null) return null
        val seconds = ((targetMs - currentMs) / 1000.0).roundToInt()
        return seconds.takeIf { it != 0 }
    }

    /**
     * The channel a voice search ("play The Office on Marathon") means: one
     * whose number is the query, else whose name is it (ignoring case),
     * else whose name contains it, among [channels] as the guide lists
     * them ([marathon.android.widget.Widgets.rows]). Null for an empty query (play whatever is on)
     * or no match.
     */
    fun searchChannel(query: String?, channels: List<WidgetChannel>): WidgetChannel? {
        val q = query?.trim()?.removePrefix("channel ")?.trim().orEmpty()
        if (q.isEmpty()) return null
        q.toIntOrNull()?.let { number -> channels.firstOrNull { it.number == number }?.let { return it } }
        return channels.firstOrNull { it.name.equals(q, ignoreCase = true) }
            ?: channels.firstOrNull { it.name.contains(q, ignoreCase = true) }
    }

    /**
     * Whether a published playback position has drifted from where
     * playback really is by more than [toleranceMs], so the session must
     * publish it again. Controllers extrapolate a published position at
     * its speed by themselves, so a playing item needs no ticking updates.
     */
    fun drifted(
        publishedMs: Long,
        publishedAtMs: Long,
        speed: Float,
        actualMs: Long,
        nowMs: Long,
        toleranceMs: Long = 1_500,
    ): Boolean {
        val expected = publishedMs + ((nowMs - publishedAtMs) * speed).toLong()
        return abs(actualMs - expected) > toleranceMs
    }
}

/**
 * When to fetch a new thumbnail of what's playing (`GET /thumbnail`):
 * straight away for a new item, then every [intervalMs] while it plays
 * on. Not while paused (the picture wouldn't change), but straight
 * away after a seek of an interval or more.
 */
class ThumbnailSchedule(private val intervalMs: Long) {
    private var itemId: String? = null
    private var positionMs = 0L
    private var atMs = 0L

    fun due(itemId: String, positionMs: Long, nowMs: Long): Boolean {
        if (itemId != this.itemId) return true
        val moved = abs(positionMs - this.positionMs) >= intervalMs / 2
        val seeked = abs(positionMs - this.positionMs) >= intervalMs
        return moved && (nowMs - atMs >= intervalMs || seeked)
    }

    /** A thumbnail of [itemId] at [positionMs] was fetched (or tried) at [nowMs]. */
    fun fetched(itemId: String, positionMs: Long, nowMs: Long) {
        this.itemId = itemId
        this.positionMs = positionMs
        atMs = nowMs
    }

    /** Forgets the last fetch, so the next is due at once. */
    fun reset() {
        itemId = null
    }
}
