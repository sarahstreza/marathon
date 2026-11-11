// A port of the tests in crates/marathon-client/src/ui.rs.
@file:Suppress("FunctionName")

package marathon.core

import marathon.api.models.ChannelRef
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
        assertNull(banner.via)
        assertEquals("Parks · S03E14 · The Injury", banner.itemLine())
        assertEquals("Parks", banner.show)
    }

    @Test
    fun nested_channels_show_the_child_before_the_title() {
        val envy = item("Contrapoints", null, null).copy(title = "Envy")
        val summary = ChannelSummary(
            id = "essayists",
            itemCount = 3,
            name = "Video Essayists",
            number = 10,
            positionMs = 0,
            current = envy,
            resolutionPath = listOf(
                ChannelRef(id = "essayists", name = "Video Essayists"),
                ChannelRef(id = "contrapoints", name = "Contrapoints"),
            ),
        )
        val banner = Banner.fromSummary(summary)
        assertEquals("Video Essayists", banner.channelName)
        // The show is named like the child channel, so it is left out.
        assertNull(banner.show)
        assertEquals("Contrapoints", banner.via)
        assertEquals("Video Essayists · Contrapoints: Envy", "${banner.channelName} · ${banner.itemLine()}")

        val guide = Guide.of(listOf(summary), 0, null)
        assertEquals("Contrapoints: Envy", guide.rows[0].on)

        // Deeper nesting lists every child.
        val deep = summary.copy(
            resolutionPath = listOf(ChannelRef("a", "A"), ChannelRef("b", "B"), ChannelRef("c", "Contrapoints")),
        )
        assertEquals("B · Contrapoints: Envy", Banner.fromSummary(deep).itemLine())
    }
}
