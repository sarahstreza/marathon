// The one activity: Setup until a server is known, then Watch. Remote keys
// become Actions here (KeyInput); Back goes through the back dispatcher;
// going to the background is standby (docs/ANDROID.md "Control channel and
// lifecycle").
package marathon.android

import android.content.pm.ActivityInfo
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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import marathon.android.ui.SetupScreen
import marathon.android.ui.WatchScreen
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
                        }
                    } else {
                        // Video is landscape: either way up, following the
                        // sensor. Setup (a form with a keyboard) follows
                        // the device. Android 16+ ignores this on large
                        // screens (tablets, foldables), where any shape works.
                        DisposableEffect(Unit) {
                            requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
                            onDispose { requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED }
                        }
                        WatchScreen(s, pendingDigits, onSettings = {
                            setupMessage = null
                            showSetup = true
                        })
                    }
                }
            }
        }
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
        super.onStop()
    }

    override fun onDestroy() {
        session?.close()
        session = null
        super.onDestroy()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) goFullscreen()
    }

    /** Fullscreen and immersive, screen kept on: it's a television. */
    private fun goFullscreen() {
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    private companion object {
        const val TAG = "Marathon"
    }
}
