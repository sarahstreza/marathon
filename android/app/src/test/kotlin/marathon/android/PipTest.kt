// The picture-in-picture window's shape: Android rejects ratios beyond
// 2.39:1 either way, so Pip.aspectRatio must always land inside them.
@file:Suppress("FunctionName")

package marathon.android

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PipTest {
    private fun ratio(aspect: Double?): Double = Pip.aspectRatio(aspect).let { (w, h) -> w.toDouble() / h }

    @Test
    fun unknown_video_is_sixteen_by_nine() {
        assertEquals(16 to 9, Pip.aspectRatio(null))
        assertEquals(16 to 9, Pip.aspectRatio(0.0))
        assertEquals(16 to 9, Pip.aspectRatio(Double.NaN))
    }

    @Test
    fun ordinary_video_keeps_its_shape() {
        assertEquals(16.0 / 9, ratio(16.0 / 9), 0.001)
        assertEquals(4.0 / 3, ratio(4.0 / 3), 0.001)
        assertEquals(9.0 / 16, ratio(9.0 / 16), 0.001)
    }

    @Test
    fun extreme_video_is_clamped_inside_androids_limits() {
        for (aspect in listOf(2.39, 2.76, 10.0, 1 / 2.39, 0.3, 0.01)) {
            val r = ratio(aspect)
            assertTrue(r <= 2.39 && r >= 1 / 2.39, "aspect $aspect gave $r")
        }
    }

    @Test
    fun video_bounds_are_the_letterboxed_picture() {
        // 16:9 on a wider phone screen: pillarboxed.
        assertEquals(listOf(240, 0, 2160, 1080), Pip.videoBounds(2400, 1080, 16.0 / 9))
        // 4:3 on 16:9: pillarboxed.
        assertEquals(listOf(240, 0, 1680, 1080), Pip.videoBounds(1920, 1080, 4.0 / 3))
        // 2.39:1 on 16:9: letterboxed.
        assertEquals(listOf(0, 138, 1920, 941), Pip.videoBounds(1920, 1080, 2.39))
    }
}
