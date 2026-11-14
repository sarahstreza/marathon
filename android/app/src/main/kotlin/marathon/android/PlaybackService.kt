// Background listening (PLAN.md M12, docs/ANDROID.md "Audio only"): a
// foreground media-playback service that runs while audio-only mode is on,
// so playback carries on with the screen off or in another app. It owns a
// MediaSession, which gives the lock screen, the notification shade,
// headset buttons, and Bluetooth controls the same three buttons as
// picture-in-picture: channel down, play/pause, and channel up. Its
// notification also has a close button, which stops background listening
// (SessionHolder.closeBackground).
//
// The service only reflects and forwards: the Session (in SessionHolder)
// plays, and every button becomes an Action from the input `media-session`.
package marathon.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.media.MediaMetadata
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Build
import android.os.IBinder
import androidx.core.content.ContextCompat
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch
import marathon.core.Action
import marathon.core.Log

class PlaybackService : Service() {
    private val scope = MainScope()
    private lateinit var mediaSession: MediaSession

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        val notifications = getSystemService(NotificationManager::class.java)
        notifications.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, getString(R.string.playback_channel), NotificationManager.IMPORTANCE_LOW).apply {
                description = getString(R.string.playback_channel_description)
                setShowBadge(false)
            },
        )
        mediaSession = MediaSession(this, "Marathon").apply {
            setCallback(
                object : MediaSession.Callback() {
                    override fun onPlay() = send(Action.Play)

                    override fun onPause() = send(Action.Pause)

                    override fun onSkipToNext() = send(Action.ChannelUp)

                    override fun onSkipToPrevious() = send(Action.ChannelDown)

                    override fun onStop() = SessionHolder.closeBackground(this@PlaybackService)

                    override fun onCustomAction(action: String, extras: android.os.Bundle?) {
                        if (action == CUSTOM_CLOSE) SessionHolder.closeBackground(this@PlaybackService)
                    }
                },
            )
            setSessionActivity(openApp())
            isActive = true
        }
        // Foreground straight away, as Android requires, then keep the
        // notification in step with what's playing.
        startInForeground(NOTIFICATION_ID, notification(null, paused = false))
        SessionHolder.serviceStarted(this)
        @OptIn(ExperimentalCoroutinesApi::class)
        scope.launch {
            SessionHolder.session
                .flatMapLatest { session ->
                    if (session == null) {
                        flowOf(null to false)
                    } else {
                        combine(session.nowPlaying, session.player.paused) { playing, paused -> playing to paused }
                    }
                }
                .collect { (playing, paused) -> show(playing, paused) }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_CHANNEL_DOWN -> send(Action.ChannelDown)
            ACTION_PLAY -> send(Action.Play)
            ACTION_PAUSE -> send(Action.Pause)
            ACTION_CHANNEL_UP -> send(Action.ChannelUp)
            ACTION_CLOSE -> SessionHolder.closeBackground(this)
        }
        // If Android kills the process, the session is gone with it: there is
        // nothing to restart.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        mediaSession.release()
        SessionHolder.serviceStopped(this)
        super.onDestroy()
    }

    private fun send(action: Action) {
        SessionHolder.session.value?.send(action, "media-session")
    }

    /** Updates the media session and the notification. */
    private fun show(playing: NowPlaying?, paused: Boolean) {
        val title = playing?.title ?: getString(R.string.app_name)
        val channel = playing?.let { "${it.channelNumber} ${it.channelName}".trim() }.orEmpty()
        mediaSession.setMetadata(
            MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, channel)
                .putString(MediaMetadata.METADATA_KEY_ALBUM, getString(R.string.app_name))
                .build(),
        )
        mediaSession.setPlaybackState(
            PlaybackState.Builder()
                .setActions(
                    PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_PLAY_PAUSE or
                        PlaybackState.ACTION_SKIP_TO_NEXT or PlaybackState.ACTION_SKIP_TO_PREVIOUS or
                        PlaybackState.ACTION_STOP,
                )
                // Android 13+ builds the notification's buttons from these.
                .addCustomAction(CUSTOM_CLOSE, getString(R.string.playback_close), R.drawable.ic_close)
                .setState(
                    if (paused) PlaybackState.STATE_PAUSED else PlaybackState.STATE_PLAYING,
                    PlaybackState.PLAYBACK_POSITION_UNKNOWN,
                    if (paused) 0f else 1f,
                )
                .build(),
        )
        try {
            getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification(playing, paused))
        } catch (e: SecurityException) {
            // No permission to post notifications: playback carries on without one.
            Log.warn("cannot update the playback notification: ${e.message}")
        }
    }

    private fun notification(playing: NowPlaying?, paused: Boolean): Notification {
        val playPause = if (paused) {
            action(ACTION_PLAY, R.drawable.ic_pip_play, R.string.pip_play)
        } else {
            action(ACTION_PAUSE, R.drawable.ic_pip_pause, R.string.pip_pause)
        }
        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(playing?.title ?: getString(R.string.app_name))
            .setContentText(playing?.let { "${it.channelNumber} ${it.channelName}".trim() })
            .setContentIntent(openApp())
            .setOngoing(true)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setCategory(Notification.CATEGORY_TRANSPORT)
            // Before Android 13 these are the notification's buttons; from 13
            // on, the media session's actions are.
            .addAction(action(ACTION_CHANNEL_DOWN, R.drawable.ic_pip_channel_down, R.string.pip_channel_down))
            .addAction(playPause)
            .addAction(action(ACTION_CHANNEL_UP, R.drawable.ic_pip_channel_up, R.string.pip_channel_up))
            .addAction(action(ACTION_CLOSE, R.drawable.ic_close, R.string.playback_close))
            .setStyle(
                Notification.MediaStyle()
                    .setMediaSession(mediaSession.sessionToken)
                    .setShowActionsInCompactView(0, 1, 2),
            )
            .build()
    }

    private fun action(name: String, icon: Int, label: Int): Notification.Action {
        val intent = Intent(this, PlaybackService::class.java).setAction(name)
        val pending = PendingIntent.getService(this, name.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE)
        return Notification.Action.Builder(Icon.createWithResource(this, icon), getString(label), pending).build()
    }

    private fun openApp(): PendingIntent = PendingIntent.getActivity(
        this,
        0,
        Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE,
    )

    private fun startInForeground(id: Int, notification: Notification) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(id, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } else {
            startForeground(id, notification)
        }
    }

    companion object {
        private const val CHANNEL_ID = "playback"
        private const val NOTIFICATION_ID = 1
        private const val CUSTOM_CLOSE = "close"
        private const val ACTION_CHANNEL_DOWN = "marathon.android.CHANNEL_DOWN"
        private const val ACTION_PLAY = "marathon.android.PLAY"
        private const val ACTION_PAUSE = "marathon.android.PAUSE"
        private const val ACTION_CHANNEL_UP = "marathon.android.CHANNEL_UP"
        private const val ACTION_CLOSE = "marathon.android.CLOSE"

        /** Starts the service. Only while the app is visible: Android refuses otherwise. */
        fun start(context: Context) {
            try {
                ContextCompat.startForegroundService(context, Intent(context, PlaybackService::class.java))
            } catch (e: IllegalStateException) {
                // ForegroundServiceStartNotAllowedException (Android 12+) is one.
                Log.warn("cannot start background playback: ${e.message}")
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, PlaybackService::class.java))
        }
    }
}
