// Picture-in-picture (docs/ANDROID.md "Picture-in-picture"). While the
// Watch screen is up, leaving the app (Home, recent apps) shrinks the video
// into a floating window that keeps playing, with channel down, play/pause,
// and channel up buttons. The activity is only paused there, not stopped,
// so playback carries on; closing the window stops it, which is standby as
// usual.
package marathon.android

import android.app.PendingIntent
import android.app.PictureInPictureParams
import android.app.RemoteAction
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.graphics.Rect
import android.graphics.drawable.Icon
import android.os.Build
import android.util.Rational
import android.view.View
import androidx.activity.ComponentActivity
import androidx.core.content.ContextCompat
import marathon.core.Action
import marathon.core.Log
import kotlin.math.ceil
import kotlin.math.floor

/**
 * Keeps [activity]'s picture-in-picture setup in step with the Watch
 * screen. The window's buttons arrive as Actions through [send]. Every
 * method runs on the main thread.
 */
class Pip(private val activity: ComponentActivity, private val send: (Action) -> Unit) {
    private val supported = activity.packageManager.hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE)

    private var watching = false
    private var paused = false
    private var aspect: Double? = null

    /** Re-measures where the video sits when the window changes size (rotation, split screen). */
    private val layoutListener = View.OnLayoutChangeListener { _, l, t, r, b, oldL, oldT, oldR, oldB ->
        if (watching && (r - l != oldR - oldL || b - t != oldB - oldT)) setParams()
    }

    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            val action = when (intent.getStringExtra(EXTRA_CONTROL)) {
                CONTROL_CHANNEL_DOWN -> Action.ChannelDown
                CONTROL_PLAY -> Action.Play
                CONTROL_PAUSE -> Action.Pause
                CONTROL_CHANNEL_UP -> Action.ChannelUp
                else -> return
            }
            send(action)
        }
    }

    /** Starts listening for the window's buttons (from onCreate). */
    fun register() {
        if (!supported) return
        ContextCompat.registerReceiver(activity, receiver, IntentFilter(ACTION_CONTROL), ContextCompat.RECEIVER_NOT_EXPORTED)
        activity.window.decorView.addOnLayoutChangeListener(layoutListener)
    }

    /** Stops listening (from onDestroy). */
    fun unregister() {
        if (!supported) return
        activity.window.decorView.removeOnLayoutChangeListener(layoutListener)
        activity.unregisterReceiver(receiver)
    }

    /**
     * The Watch screen is up ([watching]) or not, with playback [paused]
     * or not and the video's [aspect] (width / height) if known. Only the
     * Watch screen goes into picture-in-picture: Setup has nothing to show.
     */
    fun update(watching: Boolean, paused: Boolean, aspect: Double?) {
        if (!supported) return
        this.watching = watching
        this.paused = paused
        this.aspect = aspect
        setParams()
    }

    private fun setParams() {
        try {
            activity.setPictureInPictureParams(params())
        } catch (e: IllegalStateException) {
            Log.warn("cannot set picture-in-picture parameters: ${e.message}")
        }
    }

    /**
     * The viewer is leaving the app (onUserLeaveHint). Android 12+ enters
     * picture-in-picture by itself ([PictureInPictureParams.Builder.setAutoEnterEnabled]);
     * before that, the app has to ask here.
     */
    fun onUserLeaveHint() {
        if (!supported || !watching || Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) return
        try {
            activity.enterPictureInPictureMode(params())
        } catch (e: IllegalStateException) {
            // E.g. the viewer turned picture-in-picture off for Marathon in settings.
            Log.warn("cannot enter picture-in-picture: ${e.message}")
        }
    }

    private fun params(): PictureInPictureParams {
        val (width, height) = aspectRatio(aspect)
        val builder = PictureInPictureParams.Builder()
            .setAspectRatio(Rational(width, height))
            .setActions(
                listOf(
                    remoteAction(CONTROL_CHANNEL_DOWN, R.drawable.ic_pip_channel_down, R.string.pip_channel_down),
                    if (paused) {
                        remoteAction(CONTROL_PLAY, R.drawable.ic_pip_play, R.string.pip_play)
                    } else {
                        remoteAction(CONTROL_PAUSE, R.drawable.ic_pip_pause, R.string.pip_pause)
                    },
                    remoteAction(CONTROL_CHANNEL_UP, R.drawable.ic_pip_channel_up, R.string.pip_channel_up),
                ),
            )
        // Where the video is on screen, so the window grows out of it
        // smoothly. The video fills the activity, letterboxed (the
        // surface is the whole window; mpv centres the picture in it).
        val view = activity.window.decorView
        if (view.width > 0 && view.height > 0) {
            val (l, t, r, b) = videoBounds(view.width, view.height, width.toDouble() / height)
            builder.setSourceRectHint(Rect(l, t, r, b))
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            builder.setAutoEnterEnabled(watching)
            // Video shouldn't cross-fade when the window is resized.
            builder.setSeamlessResizeEnabled(false)
        }
        return builder.build()
    }

    private fun remoteAction(control: String, icon: Int, label: Int): RemoteAction {
        val intent = Intent(ACTION_CONTROL).setPackage(activity.packageName).putExtra(EXTRA_CONTROL, control)
        // One request code per control, so the PendingIntents stay distinct.
        val pending = PendingIntent.getBroadcast(
            activity,
            CONTROLS.indexOf(control),
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val title = activity.getString(label)
        return RemoteAction(Icon.createWithResource(activity, icon), title, title, pending)
    }

    companion object {
        private const val ACTION_CONTROL = "tv.marathon.android.PIP_CONTROL"
        private const val EXTRA_CONTROL = "control"
        private const val CONTROL_CHANNEL_DOWN = "channel_down"
        private const val CONTROL_PLAY = "play"
        private const val CONTROL_PAUSE = "pause"
        private const val CONTROL_CHANNEL_UP = "channel_up"
        private val CONTROLS = listOf(CONTROL_CHANNEL_DOWN, CONTROL_PLAY, CONTROL_PAUSE, CONTROL_CHANNEL_UP)

        /** Android refuses picture-in-picture windows more oblong than 2.39:1 either way. */
        private const val MAX_ASPECT = 2.39

        /** Precision of the ratio handed to Android. */
        private const val ASPECT_DENOMINATOR = 1000

        /**
         * The window's aspect ratio as width to height, for a video
         * [aspect] (width / height): 16:9 when unknown, and clamped to what
         * Android accepts (a wider film is letterboxed in the window).
         */
        fun aspectRatio(aspect: Double?): Pair<Int, Int> {
            val usable = aspect?.takeIf { it.isFinite() && it > 0 } ?: return 16 to 9
            val clamped = usable.coerceIn(1 / MAX_ASPECT, MAX_ASPECT)
            // Rounding towards 1:1 keeps the ratio inside Android's bounds.
            val scaled = clamped * ASPECT_DENOMINATOR
            val width = if (clamped > 1) floor(scaled).toInt() else ceil(scaled).toInt()
            return width to ASPECT_DENOMINATOR
        }

        /**
         * Where a video of [aspect] (width / height) shows, letterboxed or
         * pillarboxed in the middle of a [width] × [height] view: left,
         * top, right, bottom.
         */
        fun videoBounds(width: Int, height: Int, aspect: Double): List<Int> =
            if (width.toDouble() / height > aspect) {
                val w = (height * aspect).toInt()
                val left = (width - w) / 2
                listOf(left, 0, left + w, height)
            } else {
                val h = (width / aspect).toInt()
                val top = (height - h) / 2
                listOf(0, top, width, top + h)
            }
    }
}
