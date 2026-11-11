// The client's identity: a UUID generated once per installation and
// stored locally, sent as the X-Marathon-Client header (PLAN.md §8). A
// port of crates/marathon-client/src/client_id.rs; the app keeps the file
// in its private files directory.
package marathon.core

import java.io.File
import java.util.UUID

object ClientId {
    /**
     * Reads the client ID stored at [file], or generates one and stores it
     * there (creating parent directories) if the file is missing or does
     * not hold a UUID.
     */
    fun loadOrCreate(file: File): String {
        if (file.exists()) {
            val id = file.readText().trim()
            if (runCatching { UUID.fromString(id) }.isSuccess && id.length == 36) return id
            Log.warn("client ID file ${file.path} is not a UUID; replacing it")
        }
        val id = UUID.randomUUID().toString()
        file.parentFile?.mkdirs()
        file.writeText("$id\n")
        Log.info("generated a new client ID $id")
        return id
    }
}
