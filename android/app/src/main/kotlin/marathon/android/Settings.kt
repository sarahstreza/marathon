// Where the server is: the only configuration the Android client has.
// Stored in private SharedPreferences; the client ID lives beside it in a
// file (marathon.core.ClientId), as on desktop.
package marathon.android

import android.content.Context
import androidx.core.content.edit
import java.io.File
import marathon.core.ClientId

data class ServerSettings(val url: String, val token: String?)

class Settings(context: Context) {
    private val prefs = context.getSharedPreferences("marathon", Context.MODE_PRIVATE)
    private val clientIdFile = File(context.filesDir, "client-id")

    /** The saved server, if one has been set up. */
    fun server(): ServerSettings? {
        val url = prefs.getString(KEY_URL, null)?.takeIf { it.isNotBlank() } ?: return null
        return ServerSettings(url, prefs.getString(KEY_TOKEN, null)?.takeIf { it.isNotBlank() })
    }

    fun save(server: ServerSettings) {
        prefs.edit {
            putString(KEY_URL, server.url.trim())
            putString(KEY_TOKEN, server.token?.trim())
        }
    }

    /** The channel (ID) a single-channel widget shows, if one was chosen. */
    fun widgetChannel(appWidgetId: Int): String? = prefs.getString(widgetKey(appWidgetId), null)

    fun saveWidgetChannel(appWidgetId: Int, channelId: String) {
        prefs.edit { putString(widgetKey(appWidgetId), channelId) }
    }

    fun forgetWidget(appWidgetId: Int) {
        prefs.edit { remove(widgetKey(appWidgetId)) }
    }

    /** Whether audio-only mode (PLAN.md M12) was last on. */
    fun audioOnly(): Boolean = prefs.getBoolean(KEY_AUDIO_ONLY, false)

    fun saveAudioOnly(on: Boolean) {
        prefs.edit { putBoolean(KEY_AUDIO_ONLY, on) }
    }

    /** This installation's client ID (PLAN.md §8), created on first use. */
    fun clientId(): String = ClientId.loadOrCreate(clientIdFile)

    private companion object {
        const val KEY_URL = "server_url"
        const val KEY_TOKEN = "server_token"
        const val KEY_AUDIO_ONLY = "audio_only"

        fun widgetKey(appWidgetId: Int) = "widget_channel_$appWidgetId"
    }
}
