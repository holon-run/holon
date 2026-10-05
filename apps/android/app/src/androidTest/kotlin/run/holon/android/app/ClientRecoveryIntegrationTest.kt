package run.holon.android.app

import androidx.test.core.app.ActivityScenario
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Test

/** Isolated Holon fixture: never sends to a user's Agent or stores a production token. */
class ClientRecoveryIntegrationTest {
    @Test fun sessionAndDraftSurviveActivityRecreationAndRelaunch() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val device = UiDevice.getInstance(instrumentation)
        MockWebServer().use { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path.orEmpty().substringBefore('?')
                    val body = when {
                        path.endsWith("/auth/session/exchange/native") -> """{"ok":true,"credential":"fixture-session","user_id":"test-user","expires_at":null}"""
                        path.endsWith("/auth/session/me") -> """{"ok":true,"user_id":"test-user","display_name":"Tester","auth_method":"local"}"""
                        path.endsWith("/handshake") -> """{"ok":true,"protocol":{"name":"holon-control","version":1},"auth":{"mode":"bearer","required":true},"runtime":{"default_agent":"holon-tester","workspace_dir":"/test","home_dir":"/test","listen":"127.0.0.1:0","advertise_url":null},"capabilities":["agents.conversation-read.v1","auth.native-session.v1","control.prompt-idempotency.v1","control.prompt-attachments.v1","brief.attachments.v1"]}"""
                        path.endsWith("/agents/snapshot") -> """{"runtime_id":"test-runtime","event_log_epoch":"test-epoch","visibility_scope_id":"test-visibility","agents":[{"agent":{"identity":{"agent_id":"holon-tester","name":"Refactor tester","is_default_agent":true,"status":"active"},"model":{"effective_model":"test/model","runtime_default_model":"test/model","source":"runtime_default"},"status":"awake_idle","pending":0}}]}"""
                        path.endsWith("/conversation") -> """{"schema_version":2,"query_version":2,"runtime_id":"test-runtime","event_log_epoch":"test-epoch","visibility_scope_id":"test-visibility","agent_id":"holon-tester","turns":[],"active_turns":[],"pending_inputs":[],"has_more":false}"""
                        path.endsWith("/brief-read-states") -> "[]"
                        path.endsWith("/work-items") || path.endsWith("/tasks") || path.endsWith("/workspaces") -> "[]"
                        path.endsWith("/auth/session/logout") -> "{}"
                        path.endsWith("/stream") -> return MockResponse().setHeader("Content-Type", "text/event-stream")
                            .setBody(": keepalive\n\n")
                        else -> return MockResponse().setResponseCode(404).setBody("""{"code":"not_found","message":"fixture route"}""")
                    }
                    return MockResponse().setHeader("Content-Type", "application/json").setBody(body)
                }
            }
            server.start()
            val database = HolonDatabase.create(context)
            val preferences = HostPreferences(context)
            val store = createSessionStore(context)
            val repository = HolonRepository(context, store, preferences, database.holonDao(), TraceRecorder(context))
            UiCopy.select(context, "en")
            try {
                val session = runBlocking {
                    preferences.clear()
                    repository.login(server.url("/api/").toString(), "fixture-token".toCharArray(), true).first
                }
                runBlocking { repository.saveDraft("holon-tester", "durable draft") }
                var scenario = ActivityScenario.launch(MainActivity::class.java)
                try {
                    assertTrue(device.wait(Until.hasObject(By.text("Refactor tester")), 10_000))
                    device.findObject(By.text("Refactor tester")).click()
                    assertTrue(device.wait(Until.hasObject(By.text("durable draft")), 10_000))
                    scenario.recreate()
                    assertTrue(device.wait(Until.hasObject(By.text("durable draft")), 10_000))
                    scenario.close()
                    scenario = ActivityScenario.launch(MainActivity::class.java)
                    assertTrue(device.wait(Until.hasObject(By.text("Refactor tester")), 10_000))
                    device.findObject(By.text("Refactor tester")).click()
                    assertTrue(device.wait(Until.hasObject(By.text("durable draft")), 10_000))
                    device.pressBack()
                    // A conversation consumes one Back; the root Activity remains alive.
                    scenario.onActivity { assertFalse(it.isFinishing) }
                    assertEquals("durable draft", runBlocking { database.holonDao().draft(session.scopeKey, "holon-tester") })
                    assertNotNull((store as run.holon.android.sdk.ProfileSessionCredentialStore).read(session.networkId))
                } finally { scenario.close() }
            } finally {
                runBlocking { repository.logout() }
                UiCopy.select(context, null)
                database.close()
            }
        }
    }
}
