// What the home-screen widgets share (docs/ANDROID.md "Widgets"): loading
// the channel list from the saved server, the intent that opens a channel
// in the app, and asking every widget to refresh.
//
// Widgets are drawn with plain RemoteViews. They ask the server for
// GET /channels when Android updates them (every 30 minutes, the shortest
// period it allows), when one is added, and whenever the app goes to the
// background, so they show what is on after watching.
package marathon.android.widget

import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import marathon.android.MainActivity
import marathon.android.Settings
import marathon.api.models.ChannelSummary
import marathon.core.ApiError
import marathon.core.Guide
import marathon.core.HttpApi
import marathon.core.Log
import kotlin.concurrent.thread

/** A channel as a widget shows it. */
data class WidgetChannel(
    val id: String,
    val number: Int,
    val name: String,
    /** What it is on, as the guide words it, or null if it has nothing to play. */
    val on: String?,
)

/** The outcome of loading channels for a widget. */
sealed interface WidgetLoad {
    /** No server has been set up yet. */
    data object NotSetUp : WidgetLoad

    data class Failed(val message: String) : WidgetLoad

    data class Loaded(val channels: List<WidgetChannel>) : WidgetLoad
}

object Widgets {
    /** On an intent to [MainActivity]: tune to the channel with this ID. */
    const val EXTRA_CHANNEL_ID = "tv.marathon.android.CHANNEL_ID"

    /** The providers of every kind of widget, for [refreshAll]. */
    private val PROVIDERS = listOf(ChannelListWidget::class.java)

    /**
     * The channels to show, as the guide lists them: hidden channels left
     * out, in number order, each with what it is on.
     */
    fun rows(channels: List<ChannelSummary>): List<WidgetChannel> {
        val visible = channels.filter { it.hidden != true }.sortedBy { it.number }
        val guide = Guide.of(visible, highlighted = 0, current = null)
        return visible.zip(guide.rows) { c, row -> WidgetChannel(c.id, c.number, c.name, row.on) }
    }

    /** Loads the channel list from the saved server. Blocks: never call it on the main thread. */
    fun load(context: Context): WidgetLoad {
        val settings = Settings(context)
        val server = settings.server() ?: return WidgetLoad.NotSetUp
        return try {
            WidgetLoad.Loaded(rows(HttpApi(server.url, server.token, settings.clientId()).channels()))
        } catch (e: ApiError) {
            Log.warn("widget: cannot load channels: ${e.message}")
            WidgetLoad.Failed(e.message ?: "cannot reach the server")
        }
    }

    /** Opens Marathon, tuned to [channelId] if given. */
    fun openIntent(context: Context, channelId: String?): Intent =
        Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            .apply { channelId?.let { putExtra(EXTRA_CHANNEL_ID, it) } }

    /** Asks every Marathon widget on the home screen to reload. Does nothing if there are none. */
    fun refreshAll(context: Context) {
        val manager = AppWidgetManager.getInstance(context)
        for (provider in PROVIDERS) {
            val ids = manager.getAppWidgetIds(ComponentName(context, provider))
            if (ids.isEmpty()) continue
            context.sendBroadcast(
                Intent(context, provider)
                    .setAction(AppWidgetManager.ACTION_APPWIDGET_UPDATE)
                    .putExtra(AppWidgetManager.EXTRA_APPWIDGET_IDS, ids),
            )
        }
    }
}

/**
 * Runs [block] on a background thread, keeping the broadcast alive until
 * it finishes. The API client gives up after 15 s at worst, well inside
 * the minute Android gives a background broadcast.
 */
internal fun AppWidgetProvider.inBackground(block: () -> Unit) {
    val pending = goAsync()
    thread(name = "widget-update") {
        try {
            block()
        } finally {
            pending.finish()
        }
    }
}
