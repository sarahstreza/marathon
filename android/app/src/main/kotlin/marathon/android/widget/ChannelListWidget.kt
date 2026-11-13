// The channel-list widget: every channel, as the guide lists them, with
// what each is on. Tapping a channel opens Marathon tuned to it; tapping
// the title opens Marathon where it was. See Widgets.kt for when it
// refreshes.
package marathon.android.widget

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent
import android.widget.RemoteViews
import androidx.core.widget.RemoteViewsCompat
import marathon.android.R

class ChannelListWidget : AppWidgetProvider() {
    override fun onUpdate(context: Context, manager: AppWidgetManager, appWidgetIds: IntArray) {
        inBackground {
            val load = Widgets.load(context)
            for (id in appWidgetIds) manager.updateAppWidget(id, views(context, id, load))
        }
    }

    private fun views(context: Context, appWidgetId: Int, load: WidgetLoad): RemoteViews {
        val views = RemoteViews(context.packageName, R.layout.widget_channel_list)
        val openApp = PendingIntent.getActivity(
            context,
            REQUEST_OPEN_APP,
            Widgets.openIntent(context, channelId = null),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        views.setOnClickPendingIntent(R.id.header, openApp)
        views.setOnClickPendingIntent(R.id.empty, openApp)

        // Each row fills in its channel's ID; the template must be mutable
        // for that. It is explicit (MainActivity), so nothing else can
        // receive it.
        val template = PendingIntent.getActivity(
            context,
            REQUEST_OPEN_CHANNEL,
            Widgets.openIntent(context, channelId = null),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
        )
        views.setPendingIntentTemplate(R.id.list, template)
        views.setEmptyView(R.id.list, R.id.empty)

        val channels = (load as? WidgetLoad.Loaded)?.channels.orEmpty()
        val message = when (load) {
            WidgetLoad.NotSetUp -> R.string.widget_not_set_up
            is WidgetLoad.Failed -> R.string.widget_failed
            is WidgetLoad.Loaded -> R.string.widget_no_channels
        }
        views.setTextViewText(R.id.empty, context.getString(message))

        val items = RemoteViewsCompat.RemoteCollectionItems.Builder()
            .setHasStableIds(true)
            .setViewTypeCount(1)
        for (channel in channels) items.addItem(stableId(channel.id), row(context, channel))
        RemoteViewsCompat.setRemoteAdapter(context, views, appWidgetId, R.id.list, items.build())
        return views
    }

    private fun row(context: Context, channel: WidgetChannel): RemoteViews =
        RemoteViews(context.packageName, R.layout.widget_channel_row).apply {
            setTextViewText(R.id.number, channel.number.toString())
            setTextViewText(R.id.name, channel.name)
            setTextViewText(R.id.on, channel.on ?: context.getString(R.string.widget_nothing_on))
            setOnClickFillInIntent(R.id.row, Intent().putExtra(Widgets.EXTRA_CHANNEL_ID, channel.id))
        }

    private companion object {
        const val REQUEST_OPEN_APP = 0
        const val REQUEST_OPEN_CHANNEL = 1

        /** A row's ID for the list: stable per channel, so rows animate rather than redraw. */
        fun stableId(channelId: String): Long = channelId.hashCode().toLong()
    }
}
