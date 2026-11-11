// A port of the tests in crates/marathon-client/src/client_id.rs.
@file:Suppress("FunctionName")

package marathon.core

import java.io.File
import java.nio.file.Files
import java.util.UUID
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Test

class ClientIdTest {
    private val dir: File = Files.createTempDirectory("client-id").toFile().apply { deleteOnExit() }

    @Test
    fun generates_once_then_reuses() {
        val file = File(dir, "nested/dir/client-id")
        val first = ClientId.loadOrCreate(file)
        UUID.fromString(first)
        assertEquals(first, ClientId.loadOrCreate(file))
    }

    @Test
    fun replaces_garbage() {
        val file = File(dir, "client-id")
        file.writeText("not a uuid")
        val id = ClientId.loadOrCreate(file)
        assertNotEquals("not a uuid", id)
        assertEquals(id, file.readText().trim())
    }
}
