// The hand-written Action and Event types speak exactly the Rust JSON: every
// variant in the fixtures written by
// crates/marathon-api/tests/control_fixtures.rs decodes, and re-encodes to
// the same JSON. A variant added on the Rust side fails here until it is
// added to Control.kt.
@file:Suppress("FunctionName")

package marathon.core

import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

class ControlJsonTest {
    private val fixture = Json.parseToJsonElement(File(System.getProperty("marathon.controlFixture")).readText()).jsonObject

    private fun entries(key: String): JsonArray = fixture.getValue(key).jsonArray

    @Test
    fun every_action_in_the_rust_fixtures_round_trips() {
        val actions = entries("actions")
        assertTrue(actions.size >= 20, "fixture has every action: ${actions.size}")
        for (json in actions) {
            val action = ControlJson.decodeFromJsonElement(Action.serializer(), json)
            val again: JsonElement = ControlJson.encodeToJsonElement(Action.serializer(), action)
            assertEquals(json, again, "$action")
        }
    }

    @Test
    fun every_event_in_the_rust_fixtures_round_trips() {
        val events = entries("events")
        assertTrue(events.size >= 8, "fixture has every event: ${events.size}")
        for (json in events) {
            val event = ControlJson.decodeFromJsonElement(Event.serializer(), json)
            val again: JsonElement = ControlJson.encodeToJsonElement(Event.serializer(), event)
            assertEquals(json, again, "$event")
        }
    }

    @Test
    fun json_shapes() {
        assertEquals("""{"action":"channel_up"}""", Action.ChannelUp.toJson())
        assertEquals("""{"action":"go_to_channel","number":4}""", Action.GoToChannel(4).toJson())
        assertEquals("""{"action":"navigate","direction":"up"}""", Action.Navigate(Direction.Up).toJson())
        assertEquals("""{"event":"standby"}""", Event.Standby.toJson())
    }

    @Test
    fun unknown_actions_are_rejected_not_misread() {
        assertThrows<IllegalArgumentException> {
            ControlJson.decodeFromString(Action.serializer(), """{"action":"self_destruct"}""")
        }
    }
}
