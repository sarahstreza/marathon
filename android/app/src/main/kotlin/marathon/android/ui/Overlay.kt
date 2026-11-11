// The on-screen overlay: channel banner, guide, and error toast, drawn
// from OverlayModel with the desktop's fade timing (marathon.core.overlay).
// Display-only, like the desktop's egui overlay: every input arrives as an
// Action, so nothing here takes focus.
//
// Text is sized against the screen's height, so it reads the same from
// across a room on a TV and at arm's length on a phone (the desktop lays
// out on a 540-point-tall screen for the same reason).
package marathon.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.StateFlow
import marathon.android.MpvPlayer
import marathon.core.SystemClock
import marathon.core.formatMs
import marathon.core.overlay.OverlayFrame
import marathon.core.overlay.OverlayModel
import marathon.core.overlay.Wake

private val Accent = Color(0xFFF2A93B)
private val Panel = Color(0xE014161A)

/** While the banner shows, how often its progress bar moves on. */
private const val PROGRESS_REFRESH_MS = 250L

@Composable
fun OverlayLayer(model: OverlayModel, changes: StateFlow<Long>, player: MpvPlayer, pendingDigits: String) {
    val version by changes.collectAsState()
    var frame by remember { mutableStateOf(model.frame(SystemClock.nowMs())) }
    // Redraw at the display's frame rate only while something animates;
    // otherwise sleep until the next change (the PLAN's "don't spin").
    LaunchedEffect(version) {
        while (true) {
            val now = SystemClock.nowMs()
            val f = model.frame(now)
            frame = f
            when (val wake = f.wake) {
                Wake.Animating -> withFrameNanos {}
                is Wake.At -> delay(minOf(maxOf(1, wake.ms - now), if (f.bannerOpacity > 0f) PROGRESS_REFRESH_MS else Long.MAX_VALUE))
                Wake.Idle -> if (f.bannerOpacity > 0f) delay(PROGRESS_REFRESH_MS) else break
            }
        }
    }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        // One "point" per 1/540 of the screen height, as on desktop.
        val pt = maxHeight / 540f
        Banner(frame, player, pt)
        Guide(frame, pt)
        Toast(frame, pt)
        if (pendingDigits.isNotEmpty()) {
            Text(
                pendingDigits,
                color = Accent,
                fontSize = (pt * 40).toSp(),
                fontWeight = FontWeight.Bold,
                modifier = Modifier.align(Alignment.TopStart).padding(pt * 24),
            )
        }
    }
}

private fun Dp.toSp(): TextUnit = value.sp

@Composable
private fun androidx.compose.foundation.layout.BoxWithConstraintsScope.Banner(frame: OverlayFrame, player: MpvPlayer, pt: Dp) {
    val banner = frame.banner ?: return
    if (frame.bannerOpacity <= 0f) return
    Column(
        Modifier
            .align(Alignment.BottomStart)
            .fillMaxWidth()
            .alpha(frame.bannerOpacity)
            .background(Brush.verticalGradient(listOf(Color.Transparent, Color(0xD0000000))))
            .padding(horizontal = pt * 32, vertical = pt * 24),
    ) {
        Row(verticalAlignment = Alignment.Bottom) {
            Text(banner.channelNumber.toString(), color = Accent, fontSize = (pt * 40).toSp(), fontWeight = FontWeight.Bold)
            Spacer(Modifier.width(pt * 16))
            Text(banner.channelName, color = Color.White, fontSize = (pt * 28).toSp(), fontWeight = FontWeight.SemiBold, maxLines = 1)
        }
        // The child channel a nested channel is playing (PLAN.md M11):
        // "Video Essayists" above, "Contrapoints" here, then the title.
        banner.via?.let { Text("$it:", color = Color(0xFFDDDDDD), fontSize = (pt * 18).toSp(), maxLines = 1) }
        banner.show?.let { Text(it, color = Color(0xFFDDDDDD), fontSize = (pt * 18).toSp(), maxLines = 1) }
        banner.title?.let {
            Text(it, color = Color.White, fontSize = (pt * 20).toSp(), maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Spacer(Modifier.height(pt * 10))
        val position = if (frame.tuning) null else player.positionMs()
        val duration = if (frame.tuning) null else player.durationMs()
        if (position != null && duration != null && duration > 0) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                LinearProgressIndicator(
                    progress = { (position.toFloat() / duration).coerceIn(0f, 1f) },
                    color = Accent,
                    trackColor = Color(0x55FFFFFF),
                    modifier = Modifier.weight(1f).height(pt * 4),
                )
                Spacer(Modifier.width(pt * 12))
                val paused = if (player.isPaused()) "Paused  " else ""
                Text("$paused${formatMs(position)} / ${formatMs(duration)}", color = Color(0xFFDDDDDD), fontSize = (pt * 14).toSp())
            }
        } else {
            Spacer(Modifier.height(pt * 4))
        }
    }
}

@Composable
private fun androidx.compose.foundation.layout.BoxWithConstraintsScope.Guide(frame: OverlayFrame, pt: Dp) {
    val guide = frame.guide ?: return
    if (frame.guideOpacity <= 0f) return
    val list = rememberLazyListState()
    LaunchedEffect(guide.highlighted) { list.animateScrollToItem(maxOf(0, guide.highlighted - 2)) }
    LazyColumn(
        state = list,
        verticalArrangement = Arrangement.spacedBy(pt * 4),
        modifier = Modifier
            .align(Alignment.CenterStart)
            .fillMaxHeight()
            .width(maxWidth * 0.45f)
            .alpha(frame.guideOpacity)
            .background(Panel)
            .padding(pt * 16),
    ) {
        itemsIndexed(guide.rows) { i, row ->
            val highlighted = i == guide.highlighted
            Row(
                Modifier
                    .fillMaxWidth()
                    .background(if (highlighted) Accent.copy(alpha = 0.3f) else Color.Transparent, RoundedCornerShape(pt * 6))
                    .padding(horizontal = pt * 10, vertical = pt * 6),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    row.number.toString(),
                    color = if (row.current) Accent else Color.White,
                    fontSize = (pt * 20).toSp(),
                    fontWeight = FontWeight.Bold,
                    modifier = Modifier.width(pt * 48),
                )
                Column {
                    Text(row.name, color = Color.White, fontSize = (pt * 18).toSp(), maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(
                        row.on ?: "Nothing to play",
                        color = Color(0xFFBBBBBB),
                        fontSize = (pt * 14).toSp(),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

@Composable
private fun androidx.compose.foundation.layout.BoxWithConstraintsScope.Toast(frame: OverlayFrame, pt: Dp) {
    val toast = frame.toast ?: return
    if (frame.toastOpacity <= 0f) return
    Box(
        Modifier
            .align(Alignment.TopEnd)
            .padding(pt * 24)
            .widthIn(max = maxWidth * 0.5f)
            .alpha(frame.toastOpacity)
            .background(Panel, RoundedCornerShape(pt * 8))
            .padding(horizontal = pt * 16, vertical = pt * 10),
    ) {
        Text(toast, color = Color.White, fontSize = (pt * 16).toSp(), maxLines = 3, overflow = TextOverflow.Ellipsis)
    }
}
