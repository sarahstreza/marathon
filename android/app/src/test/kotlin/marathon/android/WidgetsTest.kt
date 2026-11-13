// The widgets list channels as the guide does: hidden ones left out, in
// number order, each with what it's on.
@file:Suppress("FunctionName")

package marathon.android

import marathon.android.widget.WidgetChannel
import marathon.android.widget.Widgets
import marathon.api.models.ChannelSummary
import marathon.api.models.Item
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class WidgetsTest {
    private fun channel(id: String, number: Int, current: Item? = null, hidden: Boolean? = null) =
        ChannelSummary(id = id, itemCount = 1, name = id.uppercase(), number = number, positionMs = 0, current = current, hidden = hidden)

    @Test
    fun rows_are_the_guides_channels_with_their_ids() {
        val item = Item(id = "i", missing = false, path = "p", sourceId = "s", subtitles = emptyList(), title = "Pilot", show = "Show")
        val rows = Widgets.rows(
            listOf(
                channel("b", 2),
                channel("secret", 9, hidden = true),
                channel("a", 1, current = item),
            ),
        )
        assertEquals(listOf("a", "b"), rows.map(WidgetChannel::id))
        assertEquals(listOf(1, 2), rows.map(WidgetChannel::number))
        assertEquals("A", rows[0].name)
        assertEquals(null, rows[1].on, "nothing to play")
        assertEquals(true, rows[0].on?.contains("Pilot"), rows[0].on)
    }
}
