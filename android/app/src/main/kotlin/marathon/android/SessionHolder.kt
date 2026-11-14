// The process's one Session, and what has to live as long as it rather than
// as long as an activity (docs/ANDROID.md "Audio only"):
//
// - audio-only mode, and the PlaybackService that keeps playing in the
//   background while it is on;
// - audio focus, and pausing when headphones are unplugged;
// - the sleep timer, which must still fire with the screen off.
//
// MainActivity creates the Session (it knows the server) and shows it;
// PlaybackService keeps the process, and so the Session, alive while
// listening in the background. The Session is closed once neither needs it.
//
// Everything here runs on the main thread, except `session`, which any
// thread may read.
package marathon.android

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Handler
import android.os.Looper
import androidx.core.content.ContextCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import marathon.core.Action
import marathon.core.SystemClock

object SessionHolder {
    private val handler = Handler(Looper.getMainLooper())

    private val sessionState = MutableStateFlow<Session?>(null)

    /** The current session, if a server is set up and connected to. */
    val session: StateFlow<Session?> = sessionState

    /** Called when the session quits (`Action.Quit`), to close the activity. Set by MainActivity. */
    var onQuit: (() -> Unit)? = null

    /** Whether MainActivity is started (visible). */
    private var inFront = false

    /** Whether MainActivity exists at all (it can be destroyed while the service plays on). */
    private var activityAlive = false

    /** Whether PlaybackService is running. */
    var serviceRunning = false
        private set

    // --- The session ----------------------------------------------------

    /** Replaces the session with [new] (closing the old one), and starts it. */
    fun replace(context: Context, new: Session) {
        stopService(context)
        sessionState.value?.close()
        sessionState.value = new
        new.start()
    }

    /**
     * The session quit (a relayed `Action.Quit`). Called on the tuner
     * thread: closes the activity, stops background playback, and frees
     * the session.
     */
    fun quit(context: Context) {
        handler.post {
            stopService(context)
            val activity = onQuit
            if (activity != null) activity() else close()
        }
    }

    /** Closes the session, reporting progress. */
    fun close() {
        sleepTimer.cancel()
        sessionState.value?.close()
        sessionState.value = null
    }

    // --- The activity and the service tell us where they are --------------

    fun activityCreated() {
        activityAlive = true
    }

    /** MainActivity was destroyed: the session goes too, unless the service is still playing it. */
    fun activityDestroyed() {
        activityAlive = false
        onQuit = null
        if (!serviceRunning) close()
    }

    fun activityStarted(context: Context) {
        inFront = true
        updateFocus(context)
        // Back in front with audio only on (after the notification closed the
        // service, or after the process was recreated): start listening in
        // the background again. Only now: Android lets an app start a
        // foreground service only while it is visible.
        if (sessionState.value?.player?.audioOnly?.value == true) startService(context)
    }

    /**
     * MainActivity stopped (went to the background). Returns whether to
     * stand by, as usual, or keep playing because the service is.
     */
    fun activityStopped(context: Context): Boolean {
        inFront = false
        updateFocus(context)
        return !serviceRunning
    }

    fun serviceStarted(context: Context) {
        serviceRunning = true
        updateFocus(context)
    }

    /** PlaybackService stopped. With no activity either, nothing needs the session. */
    fun serviceStopped(context: Context) {
        serviceRunning = false
        updateFocus(context)
        if (!activityAlive) close()
    }

    // --- Audio only -----------------------------------------------------

    /**
     * Turns audio-only mode on or off, and with it background playback.
     * Returns false if it can't be turned on (the server offers no audio
     * streams).
     */
    fun setAudioOnly(context: Context, on: Boolean): Boolean {
        val session = sessionState.value ?: return false
        if (!session.player.setAudioOnly(on)) return false
        if (on) startService(context) else stopService(context)
        return true
    }

    /**
     * The notification's close button: stop listening in the background.
     * Out of sight that is standby, as leaving the app usually is, so
     * coming back resumes; in sight it just pauses. Audio only stays on
     * for when the app is next in front.
     */
    fun closeBackground(context: Context) {
        stopService(context)
        sessionState.value?.send(if (inFront) Action.Pause else Action.Standby, "media-session")
    }

    // Both are harmless when the service already is (or isn't) running, and
    // `serviceRunning` lags behind them until the service is created, so
    // neither checks it.
    private fun startService(context: Context) = PlaybackService.start(context)

    private fun stopService(context: Context) = PlaybackService.stop(context)

    // --- Audio focus ------------------------------------------------------

    private var focusHeld = false

    private val focusRequest by lazy {
        AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                    .build(),
            )
            .setOnAudioFocusChangeListener { change ->
                // Polite: another app wants the speakers, so pause. Don't
                // resume by ourselves when it gives them back.
                if (change == AudioManager.AUDIOFOCUS_LOSS || change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) {
                    sessionState.value?.send(Action.Pause, "audio-focus")
                }
            }
            .build()
    }

    /** Headphones unplugged (or a Bluetooth headset gone): pause rather than play out loud. */
    private val noisy = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY) {
                sessionState.value?.send(Action.Pause, "headphones")
            }
        }
    }

    /** Holds audio focus while Marathon is in front or playing in the background. */
    private fun updateFocus(context: Context) {
        val want = inFront || serviceRunning
        if (want == focusHeld) return
        val app = context.applicationContext
        val audio = app.getSystemService(AudioManager::class.java)
        if (want) {
            audio.requestAudioFocus(focusRequest)
            ContextCompat.registerReceiver(
                app,
                noisy,
                IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY),
                ContextCompat.RECEIVER_NOT_EXPORTED,
            )
        } else {
            audio.abandonAudioFocusRequest(focusRequest)
            app.unregisterReceiver(noisy)
        }
        focusHeld = want
    }

    // --- Sleep timer ------------------------------------------------------

    private val sleepChangedState = MutableStateFlow(0)

    /** Bumped when the sleep timer is set or fires, so its button's label redraws. */
    val sleepChanged: StateFlow<Int> = sleepChangedState

    private val sleptState = MutableStateFlow(0)

    /** Bumped each time the sleep timer fires (and pauses), so the screen may turn off. */
    val slept: StateFlow<Int> = sleptState

    /** Set by MainActivity, which stops background playback when the timer fires out of sight. */
    private var appContext: Context? = null

    fun attach(context: Context) {
        appContext = context.applicationContext
    }

    val sleepTimer = SleepTimer(
        schedule = { delayMs, block ->
            val runnable = Runnable(block)
            handler.postDelayed(runnable, delayMs)
            ({ handler.removeCallbacks(runnable) })
        },
        nowMs = SystemClock::nowMs,
        onWarn = { sessionState.value?.toast("Sleep timer: pausing in a minute") },
        onSleep = {
            sleepChangedState.value += 1
            sessionState.value?.let {
                it.send(Action.Pause, "sleep-timer")
                it.toast("Sleep timer: paused")
            }
            // Asleep with the screen off: stop listening in the background too.
            val context = appContext
            if (!inFront && context != null) closeBackground(context)
            sleptState.value += 1
        },
    )

    /** The Sleep button: steps the timer to its next duration, or off. */
    fun cycleSleepTimer() {
        val minutes = sleepTimer.cycle()
        sleepChangedState.value += 1
        sessionState.value?.toast(if (minutes == null) "Sleep timer off" else "Sleep timer: $minutes min")
    }
}
