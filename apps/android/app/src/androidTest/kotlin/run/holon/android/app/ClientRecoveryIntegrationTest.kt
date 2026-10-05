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
                runBlocking { preferences.clear() }
                var scenario = ActivityScenario.launch(MainActivity::class.java)
                try {
                    assertTrue(device.wait(Until.hasObject(By.text("Connect to Holon")), 10_000))
                    device.findObjects(By.clazz("android.widget.EditText"))[0].text = server.url("/api/").toString()
                    assertTrue(device.wait(Until.hasObject(By.clazz("android.widget.CheckBox")), 10_000))
                    device.findObject(By.clazz("android.widget.CheckBox")).click()
                    val tokenField = By.clazz("android.widget.EditText").text("")
                    repeat(3) {
                        if (!device.wait(Until.hasObject(tokenField), 1_000)) {
                            device.swipe(device.displayWidth / 2, device.displayHeight * 4 / 5,
                                device.displayWidth / 2, device.displayHeight * 2 / 5, 20)
                        }
                    }
                    assertTrue(device.wait(Until.hasObject(tokenField), 10_000))
                    device.findObject(tokenField).text = "fixture-token"
                    if (device.hasObject(By.pkg("com.android.inputmethod.latin"))) device.pressBack()
                    // API 26's smaller display puts the action below the scrollable form.
                    repeat(3) {
                        if (!device.wait(Until.hasObject(By.text("Sign in")), 1_000)) {
                            device.swipe(device.displayWidth / 2, device.displayHeight * 4 / 5,
                                device.displayWidth / 2, device.displayHeight * 2 / 5, 20)
                        }
                    }
                    assertTrue(device.wait(Until.hasObject(By.text("Sign in")), 10_000))
                    device.findObject(By.text("Sign in")).click()
                    assertTrue(device.wait(Until.hasObject(By.text("Refactor tester")), 10_000))
                    val session = (runBlocking { repository.resume() } as ResumeResult.Ready).session
                    // A buffered checkpoint belongs to the exact client that opened its stream.
                    val opening = repository.sessions.capture()
                    val snapshot = run.holon.android.sdk.HolonConversationSnapshot.from(
                        run.holon.android.sdk.HolonJsonDocument(kotlinx.serialization.json.Json.parseToJsonElement(
                            """{"runtime_id":"stale-runtime","visibility_scope_id":"stale-visibility","turns":[],"pending_inputs":[]}""")),
                    )
                    val agent = run.holon.android.sdk.AgentSummary("holon-tester", "Tester", false, "active", "awake_idle", "test/model", pending = 0, currentRunId = null)
                    val previous = runBlocking { database.holonDao().conversation(session.scopeKey, agent.id) }
                    try {
                        for (replacement in listOf(session, session.copy(networkId = "another-network"))) {
                            repository.sessions.activate(replacement, run.holon.android.sdk.HolonHttpClient(server.url("/api/").toString()))
                            try {
                                runBlocking { repository.acceptConversation(agent, snapshot, opening) }
                                fail("A stale stream must be rejected before scope validation or persistence")
                            } catch (_: kotlinx.coroutines.CancellationException) { }
                            assertEquals(replacement, repository.sessions.capture().session)
                            assertNotNull((store as run.holon.android.sdk.ProfileSessionCredentialStore).read(session.networkId))
                            assertEquals(previous, runBlocking { database.holonDao().conversation(session.scopeKey, agent.id) })
                        }
                    } finally { repository.sessions.restore(opening) }
                    runBlocking { repository.saveDraft("holon-tester", "durable draft") }
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
