// Picking the channel for a single-channel widget: Android opens this when
// one is added (and, on Android 12+, when the viewer reconfigures it), and
// the widget opens it when its channel is gone. Lists the channels as the
// guide does; tapping one saves it and draws the widget.
package marathon.android.widget

import android.app.Activity
import android.appwidget.AppWidgetManager
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import marathon.android.R
import marathon.android.Settings

private val Accent = Color(0xFFF2A93B)

class ChannelWidgetConfigActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val appWidgetId = intent.getIntExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, AppWidgetManager.INVALID_APPWIDGET_ID)
        // Backing out without choosing cancels adding the widget.
        setResult(Activity.RESULT_CANCELED, result(appWidgetId))
        if (appWidgetId == AppWidgetManager.INVALID_APPWIDGET_ID) {
            finish()
            return
        }

        setContent {
            MaterialTheme(colorScheme = darkColorScheme(primary = Accent)) {
                Surface(Modifier.fillMaxSize()) {
                    // Bumped by Retry.
                    var attempt by remember { mutableIntStateOf(0) }
                    var load by remember { mutableStateOf<WidgetLoad?>(null) }
                    LaunchedEffect(attempt) {
                        load = null
                        load = withContext(Dispatchers.IO) { Widgets.load(applicationContext) }
                    }
                    Picker(load, retry = { attempt += 1 }) { channel -> choose(appWidgetId, channel, load) }
                }
            }
        }
    }

    private fun choose(appWidgetId: Int, channel: WidgetChannel, load: WidgetLoad?) {
        Settings(this).saveWidgetChannel(appWidgetId, channel.id)
        // Draw it now from what's already loaded, rather than asking the server again.
        val views = ChannelWidget.views(this, appWidgetId, channel.id, load ?: WidgetLoad.Loaded(listOf(channel)))
        AppWidgetManager.getInstance(this).updateAppWidget(appWidgetId, views)
        setResult(Activity.RESULT_OK, result(appWidgetId))
        finish()
    }

    private fun result(appWidgetId: Int) = Intent().putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, appWidgetId)
}

@Composable
private fun Picker(load: WidgetLoad?, retry: () -> Unit, onChoose: (WidgetChannel) -> Unit) {
    Column(Modifier.fillMaxSize().safeDrawingPadding().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(stringResource(R.string.widget_pick_title), style = MaterialTheme.typography.headlineSmall)
        when (load) {
            null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            WidgetLoad.NotSetUp -> Text(stringResource(R.string.widget_not_set_up))
            is WidgetLoad.Failed -> {
                Text(load.message)
                Button(onClick = retry) { Text("Retry") }
            }
            is WidgetLoad.Loaded -> if (load.channels.isEmpty()) {
                Text(stringResource(R.string.widget_no_channels))
            } else {
                LazyColumn {
                    items(load.channels, key = { it.id }) { channel ->
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .clickable { onChoose(channel) }
                                .padding(vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                channel.number.toString(),
                                color = Accent,
                                fontWeight = FontWeight.Bold,
                                style = MaterialTheme.typography.titleLarge,
                                modifier = Modifier.width(56.dp),
                            )
                            Column {
                                Text(channel.name, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                                Text(
                                    channel.on ?: stringResource(R.string.widget_nothing_on),
                                    style = MaterialTheme.typography.bodyMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}
