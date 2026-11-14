package marathon.core

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class AudioStreamTest {
    @Test
    fun urls_carry_the_start() {
        assertEquals("http://s/api/v1/audio/x?start_ms=61005", AudioStream.url("http://s/api/v1/audio/x", 61_005))
        assertEquals("http://s/a?k=v&start_ms=0", AudioStream.url("http://s/a?k=v", 0))
        assertEquals("http://s/a?start_ms=0", AudioStream.url("http://s/a", -5))
    }

    @Test
    fun positions_are_offset_by_where_the_stream_started() {
        assertEquals(60_000L, AudioStream.positionMs(60_000, 0.0))
        assertEquals(62_500L, AudioStream.positionMs(60_000, 2.5))
        assertNull(AudioStream.positionMs(60_000, null))
        assertNull(AudioStream.positionMs(60_000, Double.NaN))
        assertNull(AudioStream.positionMs(60_000, -1.0))
    }

    @Test
    fun seeks_stay_inside_the_item() {
        assertEquals(70_000L, AudioStream.seekTarget(60_000, 10, 120_000))
        assertEquals(50_000L, AudioStream.seekTarget(60_000, -10, 120_000))
        assertEquals(0L, AudioStream.seekTarget(5_000, -30, 120_000))
        // Not past the end, where there'd be nothing to stream.
        assertEquals(119_000L, AudioStream.seekTarget(115_000, 30, 120_000))
        assertEquals(0L, AudioStream.seekTarget(0, 30, 500))
        // Unknown duration: only the start bounds it.
        assertEquals(145_000L, AudioStream.seekTarget(115_000, 30, null))
    }
}
