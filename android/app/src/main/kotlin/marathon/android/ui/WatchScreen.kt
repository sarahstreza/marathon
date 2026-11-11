// The Watch screen: the video, the overlay on top, and (on touch screens)
// gestures and a control strip that send the same Actions as a remote.
package marathon.android.ui

import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import kotlinx.coroutines.delay
import marathon.android.MpvPlayer
import marathon.android.Session
import marathon.android.Touch
import marathon.core.Action

/** How long the touch control strip stays up after a tap. */
private const val CONTROLS_HOLD_MS = 4_000L

/** How far a vertical swipe must travel to change channel. */
private val SWIPE_THRESHOLD = 80.dp

@Composable
fun WatchScreen(session: Session, pendingDigits: String, onSettings: () -> Unit) {
    val send = { action: Action -> session.send(action, "touch") }
    // Bumped by each tap; the strip shows for a while after the latest.
    var controlsShownAt by remember { mutableIntStateOf(0) }
    val thresholdPx = with(LocalDensity.current) { SWIPE_THRESHOLD.toPx() }

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        VideoSurface(session.player)
        Box(
            Modifier
                .fillMaxSize()
                .pointerInput(Unit) {
                    detectTapGestures(
                        onTap = {
                            send(Action.ShowInfo)
                            controlsShownAt += 1
                        },
                        onDoubleTap = { send(Touch.doubleTap(it.x, size.width.toFloat())) },
                    )
                }
                .pointerInput(Unit) {
                    var dy = 0f
                    detectVerticalDragGestures(
                        onDragStart = { dy = 0f },
                        onDragEnd = { Touch.swipe(dy, thresholdPx)?.let(send) },
                        onVerticalDrag = { _, amount -> dy += amount },
                    )
                },
        )
        OverlayLayer(session.overlay, session.changes, session.player, pendingDigits)
        if (controlsShownAt > 0) {
            var visible by remember(controlsShownAt) { mutableIntStateOf(1) }
            LaunchedEffect(controlsShownAt) {
                delay(CONTROLS_HOLD_MS)
                visible = 0
            }
            if (visible == 1) {
                ControlStrip(send = send, onSettings = onSettings, modifier = Modifier.align(Alignment.TopCenter))
            }
        }
    }
}

@Composable
private fun ControlStrip(send: (Action) -> Unit, onSettings: () -> Unit, modifier: Modifier) {
    Row(
        modifier
            .padding(16.dp)
            .background(Color(0xC014161A), RoundedCornerShape(12.dp))
            .padding(horizontal = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        @Composable
        fun button(label: String, onClick: () -> Unit) = TextButton(onClick = onClick) { Text(label, color = Color.White) }
        button("Guide") { send(Action.ToggleGuide) }
        button("⏮") { send(Action.PreviousItem) }
        button("⏯") { send(Action.TogglePause) }
        button("⏭") { send(Action.NextItem) }
        button("Subtitles") { send(Action.ToggleSubtitles) }
        button("Server") { onSettings() }
    }
}

/** A SurfaceView that mpv draws into while it exists. */
@Composable
private fun VideoSurface(player: MpvPlayer) {
    AndroidView(
        modifier = Modifier.fillMaxSize(),
        factory = { context ->
            SurfaceView(context).apply {
                holder.addCallback(object : SurfaceHolder.Callback {
                    override fun surfaceCreated(holder: SurfaceHolder) = player.attachSurface(holder.surface)

                    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) =
                        player.surfaceSizeChanged(width, height)

                    override fun surfaceDestroyed(holder: SurfaceHolder) = player.detachSurface()
                })
            }
        },
    )
}
