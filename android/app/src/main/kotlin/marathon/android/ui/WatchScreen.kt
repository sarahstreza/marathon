// The Watch screen: the video, the overlay on top, and (on touch screens)
// gestures and a control strip that send the same Actions as a remote.
// Dragging along the banner's progress bar scrubs (ScrubBar). In
// picture-in-picture it is just the video: the window is too small for
// the overlay, and the system draws its own controls (Pip). In audio-only
// mode (PLAN.md M12) there is no video; a card says what's playing.
package marathon.android.ui

import android.content.pm.PackageManager
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.awaitTouchSlopOrCancellation
import androidx.compose.foundation.gestures.awaitVerticalTouchSlopOrCancellation
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.drag
import androidx.compose.foundation.gestures.verticalDrag
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.mandatorySystemGestures
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.systemGestures
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInWindow
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import kotlinx.coroutines.delay
import marathon.android.MpvPlayer
import marathon.android.Scrub
import marathon.android.Session
import marathon.android.SystemEdges
import marathon.android.Touch
import marathon.core.Action

/** How long the touch control strip stays up after a tap. */
private const val CONTROLS_HOLD_MS = 4_000L

/** How far a vertical swipe must travel to change channel. */
private val SWIPE_THRESHOLD = 80.dp

/** How far around the progress bar a touch still counts as on it. */
private val SCRUB_SLOP = 24.dp

/**
 * The least depth of the top and bottom strips where swipes belong to the
 * system. With the system bars hidden, or with three-button navigation,
 * the reported gesture insets can be zero, but a swipe from the very edge
 * still reveals the bars.
 */
private val MIN_SYSTEM_EDGE = 32.dp

@Composable
fun WatchScreen(
    session: Session,
    pendingDigits: String,
    inPip: Boolean,
    /** The Sleep button's label (read while drawing, so it may read state). */
    sleepLabel: () -> String,
    onSleep: () -> Unit,
    /** The Audio only button: turns the mode on or off. */
    onAudioOnly: () -> Unit,
    onSettings: () -> Unit,
) {
    val send = { action: Action -> session.send(action, "touch") }
    val audioOnly by session.player.audioOnly.collectAsState()
    // Bumped by each tap; the strip shows for a while after the latest.
    var controlsShownAt by remember { mutableIntStateOf(0) }
    val density = LocalDensity.current
    val thresholdPx = with(density) { SWIPE_THRESHOLD.toPx() }
    val gestures = WindowInsets.systemGestures
    val mandatory = WindowInsets.mandatorySystemGestures
    val minEdge = with(density) { MIN_SYSTEM_EDGE.toPx() }
    val edges = SystemEdges(
        top = maxOf(minEdge, gestures.getTop(density).toFloat(), mandatory.getTop(density).toFloat()),
        bottom = maxOf(minEdge, gestures.getBottom(density).toFloat(), mandatory.getBottom(density).toFloat()),
    )
    val scrubBar = remember { ScrubBar() }
    val scrubSlopPx = with(density) { SCRUB_SLOP.toPx() }
    // Where the gesture layer sits in the window, to find the bar in it.
    val origin = remember { object { var at = Offset.Zero } }
    // On touch screens the banner sits above the bottom system strip, so
    // its progress bar can be dragged without reaching for the system.
    val context = LocalContext.current
    val touch = remember { context.packageManager.hasSystemFeature(PackageManager.FEATURE_TOUCHSCREEN) }
    val bannerInset = if (touch) with(density) { edges.bottom.toDp() } else 0.dp

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        VideoSurface(session.player)
        if (audioOnly && !inPip) AudioCard(session)
        if (!inPip) {
            Box(
                Modifier
                    .fillMaxSize()
                    .onGloballyPositioned { origin.at = it.positionInWindow() }
                    .pointerInput(Unit) {
                        detectTapGestures(
                            onTap = {
                                send(Action.ShowInfo)
                                controlsShownAt += 1
                            },
                            onDoubleTap = { send(Touch.doubleTap(it.x, size.width.toFloat())) },
                        )
                    }
                    .pointerInput(edges) {
                        awaitEachGesture {
                            val down = awaitFirstDown(requireUnconsumed = false)
                            // A drag starting on the progress bar scrubs, whichever
                            // way it then goes (up makes it finer).
                            val bar = if (down.position.y < size.height - edges.bottom) {
                                scrubBar.hit(down.position, origin.at, scrubSlopPx)
                            } else {
                                null
                            }
                            if (bar != null) {
                                val player = session.player
                                val start = player.positionMs()
                                val duration = player.durationMs()
                                if (start == null || duration == null || duration <= 0) return@awaitEachGesture
                                // No drag (a tap): the tap handler has it.
                                val moved = awaitTouchSlopOrCancellation(down.id) { change, _ -> change.consume() }
                                    ?: return@awaitEachGesture
                                val scrub = Scrub(start, duration, bar.width, density.density)
                                var lastX = down.position.x
                                fun follow(x: Float, y: Float) {
                                    scrub.move(x - lastX, bar.center.y - y)
                                    lastX = x
                                    scrubBar.preview = ScrubPreview(scrub.targetMs, scrub.speed)
                                }
                                follow(moved.position.x, moved.position.y)
                                val lifted = try {
                                    drag(moved.id) { change ->
                                        follow(change.position.x, change.position.y)
                                        change.consume()
                                    }
                                } finally {
                                    scrubBar.preview = null
                                }
                                if (lifted) {
                                    // Relative to where playback is now: it moved on while dragging.
                                    val seconds = scrub.secondsFrom(player.positionMs() ?: start)
                                    // Even with nowhere to go, keep the banner up a while.
                                    send(if (seconds != 0) Action.SeekRelative(seconds) else Action.ShowInfo)
                                }
                                return@awaitEachGesture
                            }
                            // Otherwise a vertical drag, judged from where the finger
                            // first went down (not where it passed touch slop), so a
                            // swipe up from the bottom edge counts as the system's.
                            val slop = awaitVerticalTouchSlopOrCancellation(down.id) { change, _ -> change.consume() }
                                ?: return@awaitEachGesture
                            var lastY = slop.position.y
                            val lifted = verticalDrag(slop.id) { change ->
                                lastY = change.position.y
                                change.consume()
                            }
                            if (lifted) {
                                val dy = lastY - down.position.y
                                Touch.swipe(down.position.y, dy, size.height.toFloat(), thresholdPx, edges)?.let(send)
                            }
                        }
                    },
            )
            OverlayLayer(session.overlay, session.changes, session.player, pendingDigits, scrubBar, bannerInset)
            if (controlsShownAt > 0) {
                var visible by remember(controlsShownAt) { mutableIntStateOf(1) }
                LaunchedEffect(controlsShownAt) {
                    delay(CONTROLS_HOLD_MS)
                    visible = 0
                }
                if (visible == 1) {
                    ControlStrip(
                        send = send,
                        sleepLabel = sleepLabel(),
                        onSleep = {
                            onSleep()
                            // Keep the strip up while stepping through the durations.
                            controlsShownAt += 1
                        },
                        audioOnly = audioOnly,
                        onAudioOnly = {
                            onAudioOnly()
                            controlsShownAt += 1
                        },
                        onSettings = onSettings,
                        modifier = Modifier.align(Alignment.TopCenter),
                    )
                }
            }
        }
    }
}

/** What's playing, on black, in place of the video in audio-only mode. */
@Composable
private fun AudioCard(session: Session) {
    val playing by session.nowPlaying.collectAsState()
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("Audio only", color = Color(0xFFF2A93B), fontSize = 16.sp)
        playing?.let {
            Text(
                "${it.channelNumber} ${it.channelName}".trim(),
                color = Color(0xB3FFFFFF),
                fontSize = 20.sp,
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 12.dp),
            )
            Text(it.title, color = Color.White, fontSize = 28.sp, textAlign = TextAlign.Center)
        }
    }
}

@Composable
private fun ControlStrip(
    send: (Action) -> Unit,
    sleepLabel: String,
    onSleep: () -> Unit,
    audioOnly: Boolean,
    onAudioOnly: () -> Unit,
    onSettings: () -> Unit,
    modifier: Modifier,
) {
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
        button(sleepLabel) { onSleep() }
        button(if (audioOnly) "Video" else "Audio only") { onAudioOnly() }
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
