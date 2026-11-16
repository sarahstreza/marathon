// The media session's decisions (CarMedia) and when to refresh the
// thumbnail (ThumbnailSchedule), without Android.
@file:Suppress("FunctionName")

package marathon.android

import marathon.android.widget.WidgetChannel
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class CarMediaTest {
    @Test
    fun only_car_apps_may_browse() {
        assertTrue(CarMedia.isCarBrowser("com.google.android.projection.gearhead"))
        assertTrue(CarMedia.isCarBrowser("com.android.car.media"))
        assertFalse(CarMedia.isCarBrowser("com.android.systemui"))
        assertFalse(CarMedia.isCarBrowser("com.example.anything"))
    }

    @Test
    fun scrubbing_becomes_a_relative_seek_in_whole_seconds() {
        assertEquals(90, CarMedia.seekSeconds(100_000, 10_000))
        assertEquals(-60, CarMedia.seekSeconds(0, 60_000))
        assertEquals(2, CarMedia.seekSeconds(11_600, 10_000))
        // Already there, or nowhere to seek from.
        assertNull(CarMedia.seekSeconds(10_400, 10_000))
        assertNull(CarMedia.seekSeconds(10_000, null))
    }

    private val channels = listOf(
        WidgetChannel("office", 1, "The Office", "Pilot"),
        WidgetChannel("parks", 2, "Parks and Recreation", null),
        WidgetChannel("cartoons", 12, "Saturday Cartoons", "Ep 1"),
    )

    @Test
    fun voice_search_by_number_then_exact_name_then_part_of_one() {
        assertEquals("cartoons", CarMedia.searchChannel("12", channels)?.id)
        assertEquals("cartoons", CarMedia.searchChannel("channel 12", channels)?.id)
        assertEquals("office", CarMedia.searchChannel("the office", channels)?.id)
        assertEquals("parks", CarMedia.searchChannel("parks", channels)?.id)
        assertNull(CarMedia.searchChannel("news", channels))
        assertNull(CarMedia.searchChannel("  ", channels))
        assertNull(CarMedia.searchChannel(null, channels))
    }

    @Test
    fun a_playing_position_only_needs_republishing_when_it_drifts() {
        // Published at 10 s, playing: 5 s later it should be at 15 s.
        assertFalse(CarMedia.drifted(10_000, 0, 1f, 15_200, 5_000))
        assertTrue(CarMedia.drifted(10_000, 0, 1f, 45_000, 5_000))
        // Paused: it shouldn't move at all.
        assertFalse(CarMedia.drifted(10_000, 0, 0f, 10_000, 60_000))
        assertTrue(CarMedia.drifted(10_000, 0, 0f, 20_000, 60_000))
    }
}

class ThumbnailScheduleTest {
    private val schedule = ThumbnailSchedule(intervalMs = 30_000)

    @Test
    fun a_new_item_is_due_at_once() {
        assertTrue(schedule.due("a", 0, 0))
        schedule.fetched("a", 0, 0)
        assertTrue(schedule.due("b", 0, 1_000))
    }

    @Test
    fun playing_on_is_due_each_interval() {
        schedule.fetched("a", 0, 0)
        assertFalse(schedule.due("a", 10_000, 10_000))
        assertFalse(schedule.due("a", 29_000, 29_000))
        assertTrue(schedule.due("a", 30_000, 30_000))
    }

    @Test
    fun paused_is_never_due() {
        schedule.fetched("a", 5_000, 0)
        assertFalse(schedule.due("a", 5_000, 600_000))
    }

    @Test
    fun a_long_seek_is_due_at_once_and_a_short_one_waits() {
        schedule.fetched("a", 0, 0)
        assertTrue(schedule.due("a", 300_000, 1_000))
        assertFalse(schedule.due("a", 20_000, 1_000))
        assertTrue(schedule.due("a", 20_000, 30_000))
    }

    @Test
    fun reset_makes_the_next_due() {
        schedule.fetched("a", 0, 0)
        schedule.reset()
        assertTrue(schedule.due("a", 0, 1))
    }
}
