// A port of the tests in crates/marathon-client/src/ui.rs.
@file:Suppress("FunctionName")

package marathon.core

import marathon.api.models.ChannelSummary
import marathon.api.models.Item
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class UiTest {
    private fun item(show: String?, season: Int?, episode: Int?) = Item(
        id = "i",
        missing = false,
        path = "p.mkv",
        sourceId = "s",
        subtitles = emptyList(),
        title = "The Injury",
        show = show,
        season = season,
        episode = episode,
    )

    @Test
    fun item_titles() {
        assertEquals("S03E14 · The Injury", itemTitle(item(null, 3, 14)))
        assertEquals("E02 · The Injury", itemTitle(item(null, null, 2)))
        assertEquals("The Injury", itemTitle(item(null, 1, null)))
    }

    @Test
    fun banner_omits_a_show_named_like_the_channel() {
        fun summary(show: String) = ChannelSummary(
            id = "office",
            itemCount = 1,
            name = "The Office",
            number = 1,
            positionMs = 0,
            current = item(show, 3, 14),
        )
        var banner = Banner.fromSummary(summary("The Office"))
        assertNull(banner.show)
        assertEquals("S03E14 · The Injury", banner.title)
        banner = Banner.fromSummary(summary("Parks"))
        assertEquals("Parks", banner.show)
    }
}
