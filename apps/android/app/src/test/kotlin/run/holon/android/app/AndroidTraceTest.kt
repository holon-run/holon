package run.holon.android.app

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class AndroidTraceTest {
    @Test
    fun `redactor drops sensitive fields and limits values`() {
        val result =
            TraceRedactor.attributes(
                mapOf(
                    "Authorization" to "Bearer secret",
                    "status" to "401",
                    "detail" to "x".repeat(300),
                ),
            )

        assertFalse(result.containsKey("Authorization"))
        assertEquals("401", result["status"])
        assertEquals(160, result["detail"]?.length)
    }

    @Test
    fun `network scopes are isolated and do not expose profile id`() {
        val first = TraceScope.Network("https://one.example")
        val second = TraceScope.Network("https://two.example")

        assertTrue(first.storageKey.startsWith("network-"))
        assertFalse(first.storageKey.contains("one.example"))
        assertFalse(first.storageKey == second.storageKey)
    }

    @Test
    fun `recorder retains bounded jsonl events`() {
        val dir = Files.createTempDirectory("holon-trace-test").toFile()
        val recorder = TraceRecorder(dir, maxEvents = 2, maxBytes = 4096)
        val scope = TraceScope.Network("profile")

        recorder.record(scope, TraceLevel.INFO, "network", "first")
        recorder.record(scope, TraceLevel.INFO, "network", "second")
        recorder.record(scope, TraceLevel.ERROR, "network", "third")

        val summary = recorder.summary(scope)
        assertEquals(2, summary.eventCount)
        assertTrue(recorder.export(scope).readText().contains("holon.android.trace.v1"))
    }
}
