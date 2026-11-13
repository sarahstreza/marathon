// The single-channel widget: one channel's number and name and what it is
// on. Tapping it opens Marathon tuned to that channel. The channel is
// picked when the widget is added (ChannelWidgetConfigActivity) and kept
// in Settings by widget ID. See Widgets.kt for when it refreshes.
package marathon.android.widget

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent
import android.widget.RemoteViews
import marathon.android.R
import marathon.android.Settings

class ChannelWidget : AppWidgetProvider() {
    override fun onUpdate(context: Context, manager: AppWidgetManager, appWidgetIds: IntArray) {
        inBackground {
            val settings = Settings(context)
            val load = Widgets.load(context)
            for (id in appWidgetIds) manager.updateAppWidget(id, views(context, id, settings.widgetChannel(id), load))
        }
    }

    override fun onDeleted(context: Context, appWidgetIds: IntArray) {
        val settings = Settings(context)
        for (id in appWidgetIds) settings.forgetWidget(id)
    }

    companion object {
        /**
         * The widget [appWidgetId], showing [channelId] (null if none has
         * been picked) from what [load] found.
         */
        fun views(context: Context, appWidgetId: Int, channelId: String?, load: WidgetLoad): RemoteViews {
            val views = RemoteViews(context.packageName, R.layout.widget_channel)
            fun show(number: String, name: String, on: String) {
                views.setTextViewText(R.id.number, number)
                views.setTextViewText(R.id.name, name)
                views.setTextViewText(R.id.on, on)
            }
            val appName = context.getString(R.string.app_name)
            val channel = (load as? WidgetLoad.Loaded)?.channels?.find { it.id == channelId }
            val tap = when {
                channelId == null -> {
                    show("", appName, context.getString(R.string.widget_pick_title))
                    pickIntent(context, appWidgetId)
                }
                load == WidgetLoad.NotSetUp -> {
                    show("", appName, context.getString(R.string.widget_not_set_up))
                    openIntent(context, appWidgetId, channelId = null)
                }
                // Still worth a try: the app may reach the server when the widget couldn't.
                load is WidgetLoad.Failed -> {
                    show("", appName, context.getString(R.string.widget_failed))
                    openIntent(context, appWidgetId, channelId)
                }
                channel == null -> {
                    show("", appName, context.getString(R.string.widget_channel_gone))
                    pickIntent(context, appWidgetId)
                }
                else -> {
                    show(channel.number.toString(), channel.name, channel.on ?: context.getString(R.string.widget_nothing_on))
                    openIntent(context, appWidgetId, channelId)
                }
            }
            views.setOnClickPendingIntent(android.R.id.background, tap)
            return views
        }

        // One PendingIntent per widget (the request code), so each keeps its own channel.
        private fun openIntent(context: Context, appWidgetId: Int, channelId: String?): PendingIntent =
            PendingIntent.getActivity(
                context,
                appWidgetId,
                Widgets.openIntent(context, channelId),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )

        private fun pickIntent(context: Context, appWidgetId: Int): PendingIntent =
            PendingIntent.getActivity(
                context,
                appWidgetId,
                Intent(context, ChannelWidgetConfigActivity::class.java)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    .putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, appWidgetId),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
    }
}
