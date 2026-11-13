// The one activity: Setup until a server is known, then Watch. Remote keys
// become Actions here (KeyInput); Back goes through the back dispatcher;
// going to the background is standby (docs/ANDROID.md "Control channel and
// lifecycle"), unless the video shrinks into picture-in-picture (Pip).
package marathon.android

import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.KeyEvent
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.activity.compose.setContent
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import marathon.android.ui.SetupScreen
import marathon.android.ui.WatchScreen
import marathon.android.widget.Widgets
import marathon.core.Action
import marathon.core.ApiError
import marathon.core.Log
import marathon.core.PlayerError
import marathon.core.SystemClock

class MainActivity : ComponentActivity() {
    private lateinit var settings: Settings
    private lateinit var clientId: String
    private val handler = Handler(Looper.getMainLooper())

    private var session by mutableStateOf<Session?>(null)
    private var setupMessage by mutableStateOf<String?>(null)
    private var showSetup by mutableStateOf(false)
    private var pendingDigits by mutableStateOf("")
    private var inPip by mutableStateOf(false)

    private val pip by lazy { Pip(this) { session?.send(it, "pip") } }

    /** Bumped when the sleep timer is set or fires, so its button's label redraws. */
    private var sleepChanged by mutableIntStateOf(0)

    /** The sleep timer paused playback, so the screen may turn off until it resumes. */
    private var asleep = false

    private val sleepTimer = SleepTimer(
        schedule = { delayMs, block ->
            val runnable = Runnable(block)
            handler.postDelayed(runnable, delayMs)
            ({ handler.removeCallbacks(runnable) })
        },
        nowMs = SystemClock::nowMs,
        onWarn = { session?.toast("Sleep timer: pausing in a minute") },
        onSleep = {
            sleepChanged += 1
            session?.let {
                it.send(Action.Pause, "sleep-timer")
                it.toast("Sleep timer: paused")
            }
            asleep = true
            keepScreenOn(false)
        },
    )

    private val keys = KeyInput(
        send = { session?.send(it, "keys") },
        schedule = { delayMs, block ->
            val runnable = Runnable {
                block()
                pendingDigits = ""
            }
            handler.postDelayed(runnable, delayMs)
            ({ handler.removeCallbacks(runnable) })
        },
        nowMs = SystemClock::nowMs,
    )

    private val audioFocus by lazy {
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
                    session?.send(Action.Pause, "audio-focus")
                }
            }
            .build()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Log.sink = { level, message ->
            when (level) {
                Log.Level.Debug -> android.util.Log.d(TAG, message)
                Log.Level.Info -> android.util.Log.i(TAG, message)
                Log.Level.Warn -> android.util.Log.w(TAG, message)
                Log.Level.Error -> android.util.Log.e(TAG, message)
            }
        }
        settings = Settings(this)
        clientId = settings.clientId()
        goFullscreen()
        keepScreenOn(true)
        pip.register()

        onBackPressedDispatcher.addCallback(
            this,
            object : OnBackPressedCallback(true) {
                override fun handleOnBackPressed() {
                    val s = session
                    val now = SystemClock.nowMs()
                    val open = s != null && !showSetup && (s.overlay.guideShown(now) || s.overlay.bannerShown(now))
                    if (s != null && !showSetup && keys.onBack(open)) {
                        pendingDigits = keys.pendingDigits()
                        return
                    }
                    if (showSetup && s != null) {
                        showSetup = false
                        return
                    }
                    // Nothing to go back from: leave, as any app does.
                    isEnabled = false
                    onBackPressedDispatcher.onBackPressed()
                    isEnabled = true
                }
            },
        )

        settings.server()?.let(::connect) ?: run { showSetup = true }

        setContent {
            MaterialTheme(colorScheme = darkColorScheme(primary = Color(0xFFF2A93B))) {
                Surface(color = Color.Black) {
                    val s = session
                    if (showSetup || s == null) {
                        SetupScreen(settings.server(), setupMessage, clientId) { server ->
                            settings.save(server)
                            connect(server)
                            Widgets.refreshAll(this)
                        }
                    } else {
                        // Video is landscape: either way up, following the
                        // sensor. Setup (a form with a keyboard) follows
                        // the device. Android 16+ ignores this on large
                        // screens (tablets, foldables), where any shape works.
                        DisposableEffect(Unit) {
                            requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
                            onDispose {
                                requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
                                pip.update(watching = false, paused = false, aspect = null)
                            }
                        }
                        val paused by s.player.paused.collectAsState()
                        val aspect by s.player.videoAspect.collectAsState()
                        LaunchedEffect(paused, aspect) { pip.update(watching = true, paused, aspect) }
                        // Playing again after the sleep timer: keep the screen on again.
                        LaunchedEffect(paused) {
                            if (!paused && asleep) {
                                asleep = false
                                keepScreenOn(true)
                            }
                        }
                        WatchScreen(
                            s,
                            pendingDigits,
                            inPip,
                            sleepLabel = {
                                sleepChanged // read, so the label redraws when it changes
                                SleepTimer.label(sleepTimer.remainingMs())
                            },
                            onSleep = ::cycleSleepTimer,
                            onSettings = {
                                setupMessage = null
                                showSetup = true
                            },
                        )
                    }
                }
            }
        }

        // A widget's channel. Only on a fresh start: after the process is
        // recreated, the intent is the one that started it long ago.
        if (savedInstanceState == null) openChannelFrom(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        openChannelFrom(intent)
    }

    /**
     * Tunes to the channel a widget asked for ([Widgets.EXTRA_CHANNEL_ID]),
     * if any. On a cold start the Tuner first tunes where it would anyway
     * (the action queues behind its start), then switches.
     */
    private fun openChannelFrom(intent: Intent) {
        val channelId = intent.getStringExtra(Widgets.EXTRA_CHANNEL_ID) ?: return
        intent.removeExtra(Widgets.EXTRA_CHANNEL_ID)
        // No server yet: Setup is showing, and the widget can't have a channel anyway.
        val s = session ?: return
        showSetup = false
        s.send(Action.GoToChannelId(channelId), "widget")
    }

    /** The Sleep button: steps the timer to its next duration, or off. */
    private fun cycleSleepTimer() {
        val minutes = sleepTimer.cycle()
        sleepChanged += 1
        session?.toast(if (minutes == null) "Sleep timer off" else "Sleep timer: $minutes min")
    }

    private fun connect(server: ServerSettings) {
        session?.close()
        session = try {
            Session(this, server, clientId, onQuit = { runOnUiThread { finish() } }).also { it.start() }
        } catch (e: ApiError) {
            setupMessage = e.message
            showSetup = true
            return
        } catch (e: PlayerError) {
            setupMessage = "Cannot start the video player: ${e.message}"
            showSetup = true
            return
        }
        showSetup = false
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (session == null || showSetup) return super.onKeyDown(keyCode, event)
        val used = keys.onKeyDown(keyCode, event.repeatCount)
        pendingDigits = keys.pendingDigits()
        return used || super.onKeyDown(keyCode, event)
    }

    override fun onStart() {
        super.onStart()
        getSystemService(AudioManager::class.java).requestAudioFocus(audioFocus)
        // Back in front: resume, if going to the background was what paused it.
        session?.send(Action.Wake, "lifecycle")
    }

    override fun onStop() {
        // Going to the background: pause and report progress (PLAN.md M10).
        session?.send(Action.Standby, "lifecycle")
        getSystemService(AudioManager::class.java).abandonAudioFocusRequest(audioFocus)
        // Back on the home screen: show what the channels are on now.
        Widgets.refreshAll(this)
        super.onStop()
    }

    override fun onDestroy() {
        sleepTimer.cancel()
        pip.unregister()
        session?.close()
        session = null
        super.onDestroy()
    }

    override fun onUserLeaveHint() {
        super.onUserLeaveHint()
        pip.onUserLeaveHint()
    }

    override fun onPictureInPictureModeChanged(isInPictureInPictureMode: Boolean, newConfig: Configuration) {
        super.onPictureInPictureModeChanged(isInPictureInPictureMode, newConfig)
        inPip = isInPictureInPictureMode
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) goFullscreen()
    }

    /** Fullscreen and immersive: it's a television. */
    private fun goFullscreen() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    /** Keeps the screen on while Marathon is in front (it's a television), or lets it sleep. */
    private fun keepScreenOn(on: Boolean) {
        if (on) {
            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        } else {
            window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }

    private companion object {
        const val TAG = "Marathon"
    }
}
