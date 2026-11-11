package marathon.core

/**
 * Logging for :core, which cannot depend on Android's `android.util.Log`.
 * The app points [sink] at logcat; tests and the JVM default print to
 * stderr. Messages are short, human-readable lines, like the desktop
 * client's `tracing` output.
 */
object Log {
    enum class Level { Debug, Info, Warn, Error }

    @Volatile
    var sink: (Level, String) -> Unit = { level, message ->
        if (level >= Level.Info) System.err.println("${level.name.uppercase()} $message")
    }

    fun debug(message: String) = sink(Level.Debug, message)

    fun info(message: String) = sink(Level.Info, message)

    fun warn(message: String) = sink(Level.Warn, message)

    fun error(message: String) = sink(Level.Error, message)
}
