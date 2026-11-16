// Scrubbing on the progress bar (docs/ANDROID.md "Seeking"): the banner
// draws the bar and says where it is; the Watch screen's touch handling
// finds drags that start on it and says where they would land. ScrubBar
// is what they share.
package marathon.android.ui

import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect

/** Where a drag along the bar would land, and how fast it is moving (Scrub.speed). */
data class ScrubPreview(val targetMs: Long, val speed: Float)

@Stable
class ScrubBar {
    /** The progress bar's bounds in window coordinates, while it is on screen. */
    var bounds: Rect? = null

    /** The drag in progress along the bar, drawn by the banner. */
    var preview by mutableStateOf<ScrubPreview?>(null)

    /**
     * Whether a touch at [position] (in a view whose top left is at
     * [origin] in the window) is on the bar, give or take [slopPx] around
     * it. Returns the bar's bounds in the view's coordinates if so.
     */
    fun hit(position: Offset, origin: Offset, slopPx: Float): Rect? {
        val bar = bounds?.translate(-origin) ?: return null
        val centerY = bar.center.y
        val onBar = position.x in (bar.left - slopPx)..(bar.right + slopPx) &&
            position.y in (centerY - slopPx)..(centerY + slopPx)
        return bar.takeIf { onBar }
    }
}
