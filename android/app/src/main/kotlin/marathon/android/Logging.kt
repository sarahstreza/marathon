// Sends :core's Log to Android's logcat, under the tag "Marathon". Both
// MainActivity and PlaybackService install it, since Android Auto can
// start the service with no activity.
package marathon.android

import marathon.core.Log

private const val TAG = "Marathon"

fun installAndroidLog() {
    Log.sink = { level, message ->
        when (level) {
            Log.Level.Debug -> android.util.Log.d(TAG, message)
            Log.Level.Info -> android.util.Log.i(TAG, message)
            Log.Level.Warn -> android.util.Log.w(TAG, message)
            Log.Level.Error -> android.util.Log.e(TAG, message)
        }
    }
}
