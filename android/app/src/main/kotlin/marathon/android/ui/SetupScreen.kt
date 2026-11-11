// First-run (and "change server") screen: where is marathond, and its
// token. Test calls /health before saving, so a typo is caught here rather
// than as a silent black screen.
package marathon.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import marathon.android.ServerSettings
import marathon.core.ApiError
import marathon.core.HttpApi

@Composable
fun SetupScreen(initial: ServerSettings?, message: String?, clientId: String, onConnect: (ServerSettings) -> Unit) {
    var url by remember { mutableStateOf(initial?.url ?: "http://") }
    var token by remember { mutableStateOf(initial?.token.orEmpty()) }
    var status by remember { mutableStateOf(message) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    fun settings() = ServerSettings(url.trim(), token.trim().ifEmpty { null })

    Box(Modifier.fillMaxSize().padding(32.dp), contentAlignment = Alignment.Center) {
        Column(Modifier.widthIn(max = 560.dp).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Text("Marathon", style = MaterialTheme.typography.displaySmall)
            Text(
                "Which server should this device watch? Use the address marathond listens on, " +
                    "e.g. http://192.168.1.10:7878, and its token if it has one.",
                style = MaterialTheme.typography.bodyLarge,
            )
            OutlinedTextField(
                value = url,
                onValueChange = { url = it },
                label = { Text("Server URL") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = token,
                onValueChange = { token = it },
                label = { Text("Token (optional)") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                modifier = Modifier.fillMaxWidth(),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                OutlinedButton(enabled = !busy, onClick = {
                    busy = true
                    status = "Checking…"
                    scope.launch {
                        status = withContext(Dispatchers.IO) { check(settings(), clientId) }
                        busy = false
                    }
                }) { Text("Test") }
                Button(enabled = !busy, onClick = {
                    busy = true
                    status = "Connecting…"
                    scope.launch {
                        val problem = withContext(Dispatchers.IO) { check(settings(), clientId) }
                        busy = false
                        if (problem == OK) onConnect(settings()) else status = problem
                    }
                }) { Text("Connect") }
            }
            status?.let { Text(it, style = MaterialTheme.typography.bodyLarge) }
        }
    }
}

private const val OK = "Connected."

/** [OK], or what went wrong, in words for the viewer. */
private fun check(server: ServerSettings, clientId: String): String = try {
    HttpApi(server.url, server.token, clientId).health()
    OK
} catch (e: ApiError) {
    when {
        e is ApiError.Server && e.status == 401 -> "The server wants a token, and this one isn't it."
        else -> e.message ?: e.toString()
    }
}
