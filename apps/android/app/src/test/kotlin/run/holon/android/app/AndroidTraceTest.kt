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

    @Test
    fun `export recreates missing export directory`() {
        val dir = Files.createTempDirectory("holon-trace-export-test").toFile()
        val recorder = TraceRecorder(dir)
        val scope = TraceScope.Network("profile")

        val first = recorder.export(scope)
        assertEquals("trace-exports", first.parentFile?.name)
        first.parentFile!!.deleteRecursively()

        val second = recorder.export(scope)
        assertTrue(second.isFile)
        assertTrue(second.readText().contains("holon.android.trace.v1"))
    }

    @Test
    fun `record does not throw when trace io fails`() {
        val notADirectory = Files.createTempFile("holon-trace-io-test", ".file").toFile()
        val recorder = TraceRecorder(notADirectory)

        recorder.record(TraceScope.Global, TraceLevel.INFO, "session", "session.login")

        assertEquals(0, recorder.summary(TraceScope.Global).eventCount)
    }

    @Test
    fun `delete removes stored events for scope`() {
        val dir = Files.createTempDirectory("holon-trace-delete-test").toFile()
        val recorder = TraceRecorder(dir)
        val scope = TraceScope.Network("profile")

        recorder.record(scope, TraceLevel.INFO, "network", "network.deleted")
        assertEquals(1, recorder.summary(scope).eventCount)

        recorder.delete(scope)

        assertEquals(0, recorder.summary(scope).eventCount)
        assertFalse(java.io.File(dir, "${scope.storageKey}.jsonl").exists())
    }

    @Test
    fun `path redaction masks opaque segments but keeps endpoint segments`() {
        assertEquals(
            "/api/agents/list",
            TraceRedactor.path("http://10.0.2.2:7878/api/agents/list?token=secret"),
        )
        assertEquals(
            "/api/agents/:id/events/stream",
            TraceRedactor.path("http://10.0.2.2:7878/api/agents/holon-android/events/stream"),
        )
        assertEquals(
            "/api/agents/:id/events/stream",
            TraceRedactor.path("http://10.0.2.2:7878/api/agents/main/events/stream"),
        )
        assertEquals(
            "/api/agents/:id/conversation/stream",
            TraceRedactor.path("http://10.0.2.2:7878/api/agents/uxc-dev/conversation/stream"),
        )
        assertEquals(
            "/api/agents/:id/turns/:id/activities",
            TraceRedactor.path("http://10.0.2.2:7878/api/agents/main/turns/turn-9/activities"),
        )
        assertEquals(
            "/api/agents/:id/events/stream",
            TraceRedactor.path("http://10.0.2.2:7878/api/agents/deadbeef12345678/events/stream"),
        )
        assertEquals(
            "/api/agents/:id/events/stream",
            TraceRedactor.path("http://10.0.2.2:7878/api/agents/%E9%82%AE%E7%AE%B1%E5%8A%A9%E7%90%86/events/stream"),
        )
        assertEquals(
            "/api/auth/session/exchange/native",
            TraceRedactor.path("http://10.0.2.2:7878/api/auth/session/exchange/native"),
        )
    }

    @Test
    fun `http and sse call events are recorded with request ids and redacted paths`() {
        val dir = Files.createTempDirectory("holon-trace-http-test").toFile()
        val recorder = TraceRecorder(dir)
        val scope = TraceScope.Network("profile")

        val call = TraceHttp.started(recorder, scope, "GET", "http://host/api/agents/list")
        TraceHttp.completed(recorder, scope, call, 200)

        val sse = TraceHttp.started(recorder, scope, "GET", "http://host/api/agents/holon-android/events/stream")
        TraceHttp.failed(recorder, scope, sse, "SocketTimeoutException")

        TraceHttp.sseReconnectScheduled(recorder, scope, "agents/holon-android/events/stream", 2, 500)

        val content = java.io.File(dir, "${scope.storageKey}.jsonl").readText()
        assertTrue(content.contains("\"name\":\"http.request.started\""))
        assertTrue(content.contains("\"name\":\"http.request.completed\""))
        assertTrue(content.contains("\"statusCode\":\"200\""))
        assertTrue(content.contains("\"name\":\"sse.connect.started\""))
        assertTrue(content.contains("\"name\":\"sse.stream.failed\""))
        assertTrue(content.contains("\"errorType\":\"SocketTimeoutException\""))
        assertTrue(content.contains("\"name\":\"sse.reconnect.scheduled\""))
        assertTrue(content.contains("\"backoffMs\":\"500\""))
        assertTrue(content.contains("\"requestId\":\"${call.requestId.take(12)}\""))
        assertFalse(content.contains("holon-android"))
        assertFalse(content.contains("host"))
    }
}
