// Background listening (PLAN.md M12, docs/ANDROID.md "Audio only") and
// Android Auto (docs/ANDROID.md "Android Auto").
//
// The service owns Marathon's MediaSession, which gives the lock screen,
// the notification shade, headset buttons, Bluetooth controls, and cars
// what's playing and its controls. It runs in two ways, together or
// apart:
//
// - Listening in the background. While audio-only mode is on, the app
//   starts it as a foreground `mediaPlayback` service with a media
//   notification, so playback carries on with the screen off or in
//   another app (SessionHolder.serviceRunning). Its buttons are channel
//   down, play/pause, and channel up; a close button stops listening in
//   the background (SessionHolder.closeBackground).
//
// - Android Auto. It is a MediaBrowserService, so Android Auto (and
//   Android Automotive) bind to it, browse the channels, and pick one.
//   Only car apps get a browse root (CarMedia.CAR_BROWSERS). While one is
//   connected (SessionHolder.car), audio only is turned on, the skip
//   buttons step through the channel's items instead of channels, and
//   channel down/up become custom actions. Playing from the car starts a
//   session from the saved server if the app hasn't (startForCar), and
//   listens in the background as above.
//
// In audio-only mode, while listening in the background or in a car, the
// session's artwork is a thumbnail of what's playing (GET /thumbnail),
// refreshed every THUMBNAIL_INTERVAL_MS (ThumbnailSchedule). Servers
// without ffmpeg make none; they answer 503 each time, cheaply.
//
// The service only reflects and forwards: the Session (in SessionHolder)
// plays, and every button becomes an Action from the input `media-session`.
// Everything here runs on the main thread, except the fetches on `browseIo`
// and `thumbnailIo`, which post their results back.
package marathon.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.drawable.Icon
import android.media.MediaDescription
import android.media.MediaMetadata
import android.media.browse.MediaBrowser
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.service.media.MediaBrowserService
import androidx.core.content.ContextCompat
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch
import marathon.android.widget.WidgetLoad
import marathon.android.widget.Widgets
import marathon.core.Action
import marathon.core.ApiError
import marathon.core.Log
import marathon.core.SystemClock

class PlaybackService : MediaBrowserService() {
    private val scope = MainScope()
    private val handler = Handler(Looper.getMainLooper())
    private lateinit var mediaSession: MediaSession

    /** Loads channels for the car's browse list and voice search. */
    private val browseIo: ExecutorService = Executors.newSingleThreadExecutor { Thread(it, "car-browse").apply { isDaemon = true } }

    /** Fetches thumbnails, one at a time. */
    private val thumbnailIo: ExecutorService = Executors.newSingleThreadExecutor { Thread(it, "thumbnails").apply { isDaemon = true } }

    /** Whether the service is in the foreground, listening in the background. */
    private var foreground = false

    /** What the media session shows: the session, what's on, and whether it's paused. */
    private var session: Session? = null
    private var playing: NowPlaying? = null
    private var paused = false

    /** Why there's nothing to play, shown in the car (no server set up). */
    private var problem: String? = null

    // The last metadata and position published, to republish only on change.
    private var publishedDurationMs: Long? = null
    private var publishedPositionMs = 0L
    private var publishedAtMs = 0L
    private var publishedSpeed = 0f

    /** A thumbnail of [artItemId] (what's playing), or null. */
    private var art: Bitmap? = null
    private var artItemId: String? = null
    private val thumbnails = ThumbnailSchedule(THUMBNAIL_INTERVAL_MS)
    private var fetchingThumbnail = false

    /** The notification's buttons. Only Marathon can send to it (not exported). */
    private val buttons = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (intent.getStringExtra(EXTRA_BUTTON)) {
                BUTTON_CHANNEL_DOWN -> send(Action.ChannelDown)
                BUTTON_PLAY -> send(Action.Play)
                BUTTON_PAUSE -> send(Action.Pause)
                BUTTON_CHANNEL_UP -> send(Action.ChannelUp)
                BUTTON_CLOSE -> SessionHolder.closeBackground(this@PlaybackService)
            }
        }
    }

    override fun onCreate() {
        super.onCreate()
        installAndroidLog()
        instance = this
        val notifications = getSystemService(NotificationManager::class.java)
        notifications.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, getString(R.string.playback_channel), NotificationManager.IMPORTANCE_LOW).apply {
                description = getString(R.string.playback_channel_description)
                setShowBadge(false)
            },
        )
        mediaSession = MediaSession(this, "Marathon").apply {
            setCallback(callback)
            setSessionActivity(openApp())
            isActive = true
        }
        sessionToken = mediaSession.sessionToken
        ContextCompat.registerReceiver(this, buttons, IntentFilter(ACTION_BUTTON), ContextCompat.RECEIVER_NOT_EXPORTED)

        @OptIn(ExperimentalCoroutinesApi::class)
        scope.launch {
            SessionHolder.session
                .flatMapLatest { s ->
                    if (s == null) {
                        flowOf(Triple<Session?, NowPlaying?, Boolean>(null, null, false))
                    } else {
                        combine(s.nowPlaying, s.player.paused) { p, paused -> Triple<Session?, NowPlaying?, Boolean>(s, p, paused) }
                    }
                }
                .combine(SessionHolder.car) { state, _ -> state }
                .collect { (s, p, isPaused) ->
                    session = s
                    playing = p
                    paused = isPaused
                    if (p?.itemId != artItemId) {
                        art = null
                        artItemId = null
                    }
                    show()
                }
        }
        // Keep the position and duration in step, and the thumbnail fresh.
        scope.launch {
            while (true) {
                delay(TICK_MS)
                tick()
            }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Only start() sends this. It came through startForegroundService,
        // so the service must go to the foreground, even if it then leaves.
        goForeground()
        val s = SessionHolder.session.value
        if (s == null || !s.player.audioOnly.value) {
            Log.info("nothing to listen to in the background")
            leaveForeground()
            stopSelf(startId)
        }
        // If Android kills the process, the session is gone with it: there is
        // nothing to restart.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        unregisterReceiver(buttons)
        leaveForeground()
        SessionHolder.carDisconnected()
        mediaSession.release()
        browseIo.shutdownNow()
        thumbnailIo.shutdownNow()
        if (instance === this) instance = null
        super.onDestroy()
    }

    // --- Foreground: listening in the background ----------------------------

    private fun goForeground() {
        val notification = notification()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
        if (!foreground) {
            foreground = true
            SessionHolder.serviceStarted(this)
        }
    }

    /** Stops listening in the background. The service lives on while a car is bound to it. */
    private fun leaveForeground() {
        if (!foreground) return
        foreground = false
        stopForeground(STOP_FOREGROUND_REMOVE)
        SessionHolder.serviceStopped(this)
    }

    // --- The media session's controls ------------------------------------

    private val callback = object : MediaSession.Callback() {
        override fun onPlay() = play(null)

        override fun onPlayFromMediaId(mediaId: String, extras: Bundle?) = play(Action.GoToChannelId(mediaId))

        override fun onPlayFromSearch(query: String?, extras: Bundle?) {
            if (query.isNullOrBlank()) return play(null)
            browseIo.execute {
                val load = Widgets.load(this@PlaybackService)
                val match = (load as? WidgetLoad.Loaded)?.let { CarMedia.searchChannel(query, it.channels) }
                handler.post {
                    if (match == null) Log.info("voice search: no channel for \"$query\"")
                    play(match?.let { Action.GoToChannelId(it.id) })
                }
            }
        }

        override fun onPause() = send(Action.Pause)

        // In a car the skip buttons go through the channel's videos, and the
        // channel buttons are custom actions. Elsewhere they change channel,
        // as they always have on the lock screen and headsets.
        override fun onSkipToNext() = send(if (SessionHolder.car.value) Action.NextItem else Action.ChannelUp)

        override fun onSkipToPrevious() = send(if (SessionHolder.car.value) Action.PreviousItem else Action.ChannelDown)

        override fun onSeekTo(pos: Long) {
            val s = session ?: return
            CarMedia.seekSeconds(pos, s.player.positionMs())?.let { send(Action.SeekRelative(it)) }
            // Show the new spot straight away; the drift check corrects it.
            publishState(pos)
        }

        override fun onStop() = SessionHolder.closeBackground(this@PlaybackService)

        override fun onCustomAction(action: String, extras: Bundle?) {
            when (action) {
                CUSTOM_CLOSE -> SessionHolder.closeBackground(this@PlaybackService)
                CUSTOM_CHANNEL_DOWN -> send(Action.ChannelDown)
                CUSTOM_CHANNEL_UP -> send(Action.ChannelUp)
            }
        }
    }

    /**
     * Play, from a car or a media button: starts a session if there's none
     * (a car can ask before the app ever opened), listens in the background
     * (in a car always audio only), then sends [then] and `Play`. Android
     * lets an app start a foreground service from a media session's
     * command, which is why this can work out of sight.
     */
    private fun play(then: Action?) {
        val s = SessionHolder.startForCar(this)
        if (s == null) {
            problem = getString(R.string.widget_not_set_up)
            show()
            return
        }
        problem = null
        when {
            SessionHolder.car.value && !s.player.audioOnly.value -> SessionHolder.setAudioOnly(this, true)
            s.player.audioOnly.value && !foreground -> start(this)
        }
        then?.let { s.send(it, "media-session") }
        s.send(Action.Play, "media-session")
    }

    private fun send(action: Action) {
        SessionHolder.session.value?.send(action, "media-session")
    }

    // --- Browsing: Android Auto ----------------------------------------------

    override fun onGetRoot(clientPackageName: String, clientUid: Int, rootHints: Bundle?): BrowserRoot? {
        // The framework has already checked that the package belongs to the caller.
        if (!CarMedia.isCarBrowser(clientPackageName)) {
            Log.info("$clientPackageName may not browse Marathon")
            return null
        }
        SessionHolder.carConnected(this)
        if (Settings(this).server() == null) {
            problem = getString(R.string.widget_not_set_up)
            show()
        }
        val extras = Bundle().apply {
            putBoolean(CONTENT_STYLE_SUPPORTED, true)
            putInt(CONTENT_STYLE_PLAYABLE_HINT, CONTENT_STYLE_LIST)
        }
        return BrowserRoot(ROOT_ID, extras)
    }

    override fun onUnbind(intent: Intent?): Boolean {
        // Every browser (car) has gone.
        if (intent?.action == SERVICE_INTERFACE) SessionHolder.carDisconnected()
        return super.onUnbind(intent)
    }

    override fun onLoadChildren(parentId: String, result: Result<MutableList<MediaBrowser.MediaItem>>) {
        when (parentId) {
            // One tab, "Channels". Cars show a root of browsable items as tabs.
            ROOT_ID -> result.sendResult(
                mutableListOf(
                    MediaBrowser.MediaItem(
                        MediaDescription.Builder().setMediaId(CHANNELS_ID).setTitle(getString(R.string.car_channels)).build(),
                        MediaBrowser.MediaItem.FLAG_BROWSABLE,
                    ),
                ),
            )
            CHANNELS_ID -> {
                result.detach()
                browseIo.execute {
                    val items = when (val load = Widgets.load(this)) {
                        is WidgetLoad.Loaded -> load.channels.map { channel ->
                            MediaBrowser.MediaItem(
                                MediaDescription.Builder()
                                    .setMediaId(channel.id)
                                    .setTitle("${channel.number}  ${channel.name}")
                                    .setSubtitle(channel.on ?: getString(R.string.widget_nothing_on))
                                    .build(),
                                MediaBrowser.MediaItem.FLAG_PLAYABLE,
                            )
                        }
                        else -> emptyList()
                    }
                    handler.post { result.sendResult(items.toMutableList()) }
                }
            }
            else -> result.sendResult(mutableListOf())
        }
    }

    // --- What the session shows -----------------------------------------------

    /** Updates the media session, and the notification while in the foreground. */
    private fun show() {
        showMetadata()
        publishState(session?.player?.positionMs())
        if (!foreground) return
        try {
            getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification())
        } catch (e: SecurityException) {
            // No permission to post notifications: playback carries on without one.
            Log.warn("cannot update the playback notification: ${e.message}")
        }
    }

    private fun showMetadata() {
        val p = playing
        val title = p?.title ?: getString(R.string.app_name)
        val channel = channelLine(p)
        val duration = session?.player?.durationMs()
        publishedDurationMs = duration
        mediaSession.setMetadata(
            MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, channel)
                .putString(MediaMetadata.METADATA_KEY_ALBUM, getString(R.string.app_name))
                .putString(MediaMetadata.METADATA_KEY_DISPLAY_TITLE, title)
                .putString(MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE, channel)
                .apply {
                    p?.itemId?.let { putString(MediaMetadata.METADATA_KEY_MEDIA_ID, it) }
                    // With a duration (and ACTION_SEEK_TO), cars and the lock screen show a seek bar.
                    duration?.let { putLong(MediaMetadata.METADATA_KEY_DURATION, it) }
                    art?.let {
                        putBitmap(MediaMetadata.METADATA_KEY_ART, it)
                        putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, it)
                    }
                }
                .build(),
        )
    }

    /** Publishes the playback state, at [positionMs] if known. */
    private fun publishState(positionMs: Long?) {
        val s = session
        val car = SessionHolder.car.value
        val state = when {
            problem != null && s == null -> PlaybackState.STATE_ERROR
            s == null || playing == null -> PlaybackState.STATE_STOPPED
            paused -> PlaybackState.STATE_PAUSED
            else -> PlaybackState.STATE_PLAYING
        }
        val speed = if (state == PlaybackState.STATE_PLAYING) 1f else 0f
        val now = android.os.SystemClock.elapsedRealtime()
        val builder = PlaybackState.Builder()
            .setActions(
                PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_PLAY_PAUSE or
                    PlaybackState.ACTION_SKIP_TO_NEXT or PlaybackState.ACTION_SKIP_TO_PREVIOUS or
                    PlaybackState.ACTION_STOP or PlaybackState.ACTION_SEEK_TO or
                    PlaybackState.ACTION_PLAY_FROM_MEDIA_ID or PlaybackState.ACTION_PLAY_FROM_SEARCH,
            )
            .setState(state, positionMs ?: PlaybackState.PLAYBACK_POSITION_UNKNOWN, speed, now)
        if (state == PlaybackState.STATE_ERROR) builder.setErrorMessage(problem)
        // Android 13+ builds the notification's extra buttons from these, as
        // cars do. In a car, the skip buttons are for videos, so the channel
        // buttons go here; elsewhere, the close button does.
        if (car) {
            builder.addCustomAction(CUSTOM_CHANNEL_DOWN, getString(R.string.pip_channel_down), R.drawable.ic_pip_channel_down)
            builder.addCustomAction(CUSTOM_CHANNEL_UP, getString(R.string.pip_channel_up), R.drawable.ic_pip_channel_up)
        } else {
            builder.addCustomAction(CUSTOM_CLOSE, getString(R.string.playback_close), R.drawable.ic_close)
        }
        mediaSession.setPlaybackState(builder.build())
        publishedPositionMs = positionMs ?: 0
        publishedAtMs = now
        publishedSpeed = speed
    }

    /** Once a second: republish a drifted position or a newly known duration, and fetch a thumbnail when due. */
    private fun tick() {
        val s = session ?: return
        if (playing == null) return
        val duration = s.player.durationMs()
        if (duration != publishedDurationMs) showMetadata()
        val position = s.player.positionMs() ?: return
        val now = android.os.SystemClock.elapsedRealtime()
        if (CarMedia.drifted(publishedPositionMs, publishedAtMs, publishedSpeed, position, now)) publishState(position)
        maybeFetchThumbnail(s, position)
    }

    // --- Thumbnails ------------------------------------------------------

    private fun maybeFetchThumbnail(s: Session, position: Long) {
        val itemId = playing?.itemId ?: return
        val wanted = s.player.audioOnly.value && (foreground || SessionHolder.car.value)
        if (!wanted || fetchingThumbnail) return
        val now = SystemClock.nowMs()
        if (!thumbnails.due(itemId, position, now)) return
        thumbnails.fetched(itemId, position, now)
        fetchingThumbnail = true
        thumbnailIo.execute {
            val picture = try {
                val bytes = s.api.thumbnail(itemId, position, THUMBNAIL_WIDTH)
                BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
            } catch (e: ApiError) {
                // A server without ffmpeg makes none (503, cheaply); a busy
                // one, or a frame past the end, may next time. Either way the
                // schedule asks again an interval later.
                Log.debug("no thumbnail: ${e.message}")
                null
            }
            handler.post {
                fetchingThumbnail = false
                // Still what's playing? Then show it; else the next tick fetches anew.
                if (picture != null && session === s && playing?.itemId == itemId) {
                    art = picture
                    artItemId = itemId
                    show()
                }
            }
        }
    }

    // --- The notification --------------------------------------------------

    private fun channelLine(p: NowPlaying?): String = p?.let { "${it.channelNumber} ${it.channelName}".trim() }.orEmpty()

    private fun notification(): Notification {
        val p = playing
        val playPause = if (paused) {
            button(BUTTON_PLAY, R.drawable.ic_pip_play, R.string.pip_play)
        } else {
            button(BUTTON_PAUSE, R.drawable.ic_pip_pause, R.string.pip_pause)
        }
        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setLargeIcon(art)
            .setContentTitle(p?.title ?: getString(R.string.app_name))
            .setContentText(p?.let(::channelLine))
            .setContentIntent(openApp())
            .setOngoing(true)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setCategory(Notification.CATEGORY_TRANSPORT)
            // Before Android 13 these are the notification's buttons; from 13
            // on, the media session's actions are.
            .addAction(button(BUTTON_CHANNEL_DOWN, R.drawable.ic_pip_channel_down, R.string.pip_channel_down))
            .addAction(playPause)
            .addAction(button(BUTTON_CHANNEL_UP, R.drawable.ic_pip_channel_up, R.string.pip_channel_up))
            .addAction(button(BUTTON_CLOSE, R.drawable.ic_close, R.string.playback_close))
            .setStyle(
                Notification.MediaStyle()
                    .setMediaSession(mediaSession.sessionToken)
                    .setShowActionsInCompactView(0, 1, 2),
            )
            .build()
    }

    private fun button(name: String, icon: Int, label: Int): Notification.Action {
        val intent = Intent(ACTION_BUTTON).setPackage(packageName).putExtra(EXTRA_BUTTON, name)
        val pending = PendingIntent.getBroadcast(this, name.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE)
        return Notification.Action.Builder(Icon.createWithResource(this, icon), getString(label), pending).build()
    }

    private fun openApp(): PendingIntent = PendingIntent.getActivity(
        this,
        0,
        Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE,
    )

    companion object {
        private const val CHANNEL_ID = "playback"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_BUTTON = "marathon.android.PLAYBACK_BUTTON"
        private const val EXTRA_BUTTON = "button"
        private const val BUTTON_CHANNEL_DOWN = "channel_down"
        private const val BUTTON_PLAY = "play"
        private const val BUTTON_PAUSE = "pause"
        private const val BUTTON_CHANNEL_UP = "channel_up"
        private const val BUTTON_CLOSE = "close"
        private const val CUSTOM_CLOSE = "close"
        private const val CUSTOM_CHANNEL_DOWN = "channel_down"
        private const val CUSTOM_CHANNEL_UP = "channel_up"

        private const val ROOT_ID = "root"
        private const val CHANNELS_ID = "channels"

        // Content-style hints that cars read from the browse root's extras.
        private const val CONTENT_STYLE_SUPPORTED = "android.media.browse.CONTENT_STYLE_SUPPORTED"
        private const val CONTENT_STYLE_PLAYABLE_HINT = "android.media.browse.CONTENT_STYLE_PLAYABLE_HINT"
        private const val CONTENT_STYLE_LIST = 1

        private const val TICK_MS = 1_000L

        /** How often the thumbnail of what's playing is refreshed. */
        const val THUMBNAIL_INTERVAL_MS = 30_000L

        /** Thumbnail width in pixels: enough for a car screen, small enough for a media session. */
        private const val THUMBNAIL_WIDTH = 480

        /** The running service, if any. Main thread only. */
        private var instance: PlaybackService? = null

        /**
         * Starts listening in the background. Only while the app is visible,
         * or from a media session command: Android refuses otherwise.
         */
        fun start(context: Context) {
            try {
                ContextCompat.startForegroundService(context, Intent(context, PlaybackService::class.java))
            } catch (e: IllegalStateException) {
                // ForegroundServiceStartNotAllowedException (Android 12+) is one.
                Log.warn("cannot start background playback: ${e.message}")
            }
        }

        /** Stops listening in the background. A car may keep the service itself bound. */
        fun stop(context: Context) {
            instance?.leaveForeground()
            context.stopService(Intent(context, PlaybackService::class.java))
        }
    }
}
