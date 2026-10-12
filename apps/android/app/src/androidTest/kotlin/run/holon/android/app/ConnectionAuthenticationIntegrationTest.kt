package run.holon.android.app

import android.app.Application
import android.content.Context
import android.content.SharedPreferences
import android.net.Uri
import android.util.Base64
import android.view.inputmethod.InputMethodManager
import androidx.activity.compose.setContent
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.viewModelScope
import androidx.room.Room
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import java.io.File
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import run.holon.android.sdk.HolonAuthenticationMethod
import run.holon.android.sdk.SessionCredentialStore

/** All requests and credentials belong to isolated fixtures, never a real Holon server. */
@RunWith(AndroidJUnit4::class)
class ConnectionAuthenticationIntegrationTest {
    @Test
    fun firstScreenHasAnEmptyAddressAndScanWithoutAuthenticationControls() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val preferences = HostPreferences(context)
        val device = UiDevice.getInstance(instrumentation)
        runBlocking { preferences.clear() }
        UiCopy.select(context, "en")
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                assertTrue(device.wait(Until.hasObject(By.text("Connect to Holon")), 10_000))
                val fields = device.findObjects(By.clazz("android.widget.EditText"))
                assertEquals(1, fields.size)
                assertEquals("", fields.single().text)
                assertFalse(device.hasObject(By.text("Access token")))
                assertFalse(device.hasObject(By.text("Sign in")))
                assertFalse(device.hasObject(By.text(ui("使用组织浏览器登录"))))
                scenario.onActivity { activity ->
                    activity.getSystemService(InputMethodManager::class.java)
                        .hideSoftInputFromWindow(activity.window.decorView.windowToken, 0)
                }
                scrollTo(device, "Scan connection QR code")
                scrollTo(device, "Next")
                assertFalse(device.hasObject(By.text("Access token")))
                assertFalse(device.hasObject(By.text(ui("使用组织浏览器登录"))))
            }
        } finally {
            runBlocking { preferences.clear() }
            UiCopy.select(context, null)
        }
    }

    @Test
    fun localTokenDiscoveryIsAnonymousAndChangeAddressClearsTheToken() {
        Fixture("""{"mode":"local"}""").use { fixture ->
            fixture.continueLogin()
            val authenticatedStep = fixture.awaitState {
                !it.busy && it.authenticationMethod == HolonAuthenticationMethod.LocalToken
            }
            assertEquals("", authenticatedStep.token)
            assertNull(authenticatedStep.browserLoginUrl)
            fixture.assertAnonymousProbe()

            fixture.onMain {
                setToken("fixture-token")
                toggleToken()
                returnToConnectionAddress()
            }
            val addressStep = fixture.viewModel.state.value
            assertEquals(AppPhase.SignedOut, addressStep.phase)
            assertEquals(fixture.server.url("/api/").toString(), addressStep.baseUrl)
            assertEquals("", addressStep.token)
            assertFalse(addressStep.showToken)
            assertNull(addressStep.authenticationMethod)
            assertNull(addressStep.browserLoginUrl)
            assertNull(addressStep.error)
            assertEquals(1, fixture.server.requestCount)
        }
    }

    @Test
    fun changingTheServerAddressResetsAuthenticationAndHttpConsent() {
        Fixture("""{"mode":"local"}""").use { fixture ->
            fixture.continueLogin()
            fixture.awaitState { !it.busy && it.authenticationMethod == HolonAuthenticationMethod.LocalToken }
            fixture.onMain {
                setToken("fixture-token")
                toggleToken()
                setBaseUrl("https://another.fixture.invalid")
            }
            val addressStep = fixture.viewModel.state.value
            assertEquals("https://another.fixture.invalid", addressStep.baseUrl)
            assertEquals("", addressStep.token)
            assertFalse(addressStep.showToken)
            assertFalse(addressStep.allowInsecureHttp)
            assertNull(addressStep.authenticationMethod)
            assertNull(addressStep.browserLoginUrl)
            fixture.assertAnonymousProbe()
        }
    }

    @Test
    fun lateAuthenticationResponsesDoNotOverwriteANewAddressAuthenticationStep() {
        for (responseCode in listOf(200, 503)) {
            Fixture("""{"mode":"oidc"}""", responseCode, delayAuthenticationResponse = true).use { fixture ->
                val oldProbe = fixture.continueLogin()
                fixture.assertAnonymousProbe()
                MockWebServer().use { replacement ->
                    replacement.enqueue(MockResponse().setHeader("Content-Type", "application/json")
                        .setBody("""{"mode":"local"}"""))
                    replacement.start()
                    val replacementAddress = replacement.url("/api/").toString()
                    fixture.onMain { setBaseUrl(replacementAddress) }
                    val addressStep = fixture.viewModel.state.value
                    assertEquals(replacementAddress, addressStep.baseUrl)
                    assertFalse(addressStep.busy)
                    assertFalse(addressStep.allowInsecureHttp)
                    assertNull(addressStep.authenticationMethod)
                    assertNull(addressStep.browserLoginUrl)
                    assertNull(addressStep.error)
                    assertNull(addressStep.statusMessage)

                    fixture.onMain {
                        setAllowInsecureHttp(true)
                        continueLogin()
                    }
                    fixture.awaitState { !it.busy && it.authenticationMethod == HolonAuthenticationMethod.LocalToken }
                    fixture.onMain { setToken("replacement-fixture-token") }
                    val replacementStep = fixture.viewModel.state.value
                    val request = replacement.takeRequest(2, TimeUnit.SECONDS)
                    assertNotNull(request)
                    assertEquals("GET", request!!.method)
                    assertEquals("/api/auth/method", request.path)
                    assertNull(request.getHeader("Authorization"))
                    assertNull(request.getHeader("Cookie"))

                    fixture.releaseAuthenticationResponse()
                    fixture.awaitProbeCompletion(oldProbe)
                    assertEquals(replacementStep, fixture.viewModel.state.value)
                    assertEquals(1, replacement.requestCount)
                    assertEquals(1, fixture.server.requestCount)
                }
            }
        }
    }

    @Test
    fun lateAuthenticationResponsesDoNotRestoreRevokedHttpConsent() {
        for (responseCode in listOf(200, 503)) {
            Fixture("""{"mode":"oidc"}""", responseCode, delayAuthenticationResponse = true).use { fixture ->
                val oldProbe = fixture.continueLogin()
                fixture.assertAnonymousProbe()
                fixture.onMain { setAllowInsecureHttp(false) }
                val addressStep = fixture.viewModel.state.value
                assertEquals(AppPhase.SignedOut, addressStep.phase)
                assertEquals(fixture.server.url("/api/").toString(), addressStep.baseUrl)
                assertFalse(addressStep.busy)
                assertFalse(addressStep.allowInsecureHttp)
                assertEquals("", addressStep.token)
                assertFalse(addressStep.showToken)
                assertNull(addressStep.authenticationMethod)
                assertNull(addressStep.browserLoginUrl)
                assertNull(addressStep.error)
                assertNull(addressStep.statusMessage)

                fixture.releaseAuthenticationResponse()
                fixture.awaitProbeCompletion(oldProbe)
                assertEquals(addressStep, fixture.viewModel.state.value)
                assertEquals(1, fixture.server.requestCount)
            }
        }
    }

    @Test
    fun oidcDiscoveryProducesAS256BrowserUrlWithoutATokenExchange() {
        Fixture("""{"mode":"oidc"}""").use { fixture ->
            fixture.continueLogin()
            val browserStep = fixture.awaitState { !it.busy && it.browserLoginUrl != null }
            assertEquals(HolonAuthenticationMethod.Oidc, browserStep.authenticationMethod)
            assertEquals("", browserStep.token)
            assertFalse(browserStep.showToken)
            assertNull(browserStep.error)
            val loginUrl = Uri.parse(browserStep.browserLoginUrl!!)
            val endpoint = Uri.parse(fixture.server.url("/api/auth/oidc/native/start").toString())
            assertEquals(endpoint.scheme, loginUrl.scheme)
            assertEquals(endpoint.host, loginUrl.host)
            assertEquals(endpoint.port, loginUrl.port)
            assertEquals(endpoint.path, loginUrl.path)
            assertEquals(setOf("state", "code_challenge", "code_challenge_method"), loginUrl.queryParameterNames)
            assertEquals("S256", loginUrl.getQueryParameter("code_challenge_method"))
            val proof = createSessionStore(fixture.application).read("oidc-login-proof")
            assertNotNull(proof)
            val challenge = Base64.encodeToString(
                MessageDigest.getInstance("SHA-256").digest(proof!!.toByteArray(Charsets.US_ASCII)),
                Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING,
            )
            assertEquals(challenge, loginUrl.getQueryParameter("code_challenge"))
            val loginState = loginUrl.getQueryParameter("state")
            assertFalse(loginState.isNullOrBlank())
            assertEquals(
                loginState,
                fixture.application.getSharedPreferences("holon_oidc_login", Context.MODE_PRIVATE)
                    .getString("state", null),
            )
            fixture.assertAnonymousProbe()

            fixture.onMain { browserLoginHandled() }
            assertNull(fixture.viewModel.state.value.browserLoginUrl)
            assertEquals(HolonAuthenticationMethod.Oidc, fixture.viewModel.state.value.authenticationMethod)
            assertEquals("", fixture.viewModel.state.value.token)
            assertEquals(1, fixture.server.requestCount)
            assertOidcScreenHasNoTokenFallback(fixture)
        }
    }

    @Test
    fun authenticationHttpErrorsDoNotEnterTheAuthenticationStep() {
        for (responseCode in listOf(401, 403, 503)) {
            Fixture("""{"code":"unavailable","message":"fixture unavailable"}""", responseCode).use { fixture ->
                fixture.continueLogin()
                fixture.assertRejectedAuthentication()
            }
        }
    }

    @Test
    fun unknownAuthenticationModesDoNotFallBackToLocalTokens() {
        Fixture("""{"mode":"future_authentication"}""").use { fixture ->
            fixture.continueLogin()
            fixture.assertRejectedAuthentication()
        }
    }

    @Test
    fun malformedAuthenticationMethodsDoNotFallBackToLocalTokens() {
        for (body in listOf("{}", """{"mode":null}""", """{"mode":7}""", "[]")) {
            Fixture(body).use { fixture ->
                fixture.continueLogin()
                fixture.assertRejectedAuthentication()
            }
        }
    }

    private fun scrollTo(device: UiDevice, text: String) {
        repeat(3) {
            if (!device.wait(Until.hasObject(By.text(text)), 1_000)) {
                device.swipe(device.displayWidth / 2, device.displayHeight * 4 / 5,
                    device.displayWidth / 2, device.displayHeight * 2 / 5, 20)
            }
        }
        if (!device.wait(Until.hasObject(By.text(text)), 10_000)) {
            val visibleTexts = device.findObjects(By.clazz("android.widget.TextView")).mapNotNull { it.text }
            fail("Expected visible text \"$text\", found: $visibleTexts")
        }
    }

    private fun assertOidcScreenHasNoTokenFallback(fixture: Fixture) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val preferences = HostPreferences(context)
        val device = UiDevice.getInstance(instrumentation)
        runBlocking { preferences.clear() }
        UiCopy.select(context, "en")
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                scenario.onActivity { activity ->
                    // Consume the launch event before rendering: the test must not open a browser.
                    val state = fixture.viewModel.state.value
                    assertNull(state.browserLoginUrl)
                    activity.setContent { LoginScreen(state, fixture.viewModel.screenActions.connection) }
                }
                scrollTo(device, ui("使用组织浏览器登录"))
                assertFalse(device.hasObject(By.clazz("android.widget.EditText")))
                assertFalse(device.hasObject(By.text("Access token")))
                assertFalse(device.hasObject(By.text("Sign in")))
                assertFalse(device.hasObject(By.text("Next")))
                assertTrue(device.hasObject(By.text("Change address")))
            }
        } finally {
            runBlocking { preferences.clear() }
            UiCopy.select(context, null)
        }
    }

    private class Fixture(
        body: String,
        responseCode: Int = 200,
        delayAuthenticationResponse: Boolean = false,
    ) : AutoCloseable {
        private val instrumentation = InstrumentationRegistry.getInstrumentation()
        private val root = File(instrumentation.targetContext.cacheDir, "connection-auth-${UUID.randomUUID()}")
            .apply { check(mkdirs()) }
        val application = FixtureApplication(instrumentation.targetContext, root)
        val server = MockWebServer()
        private val authenticationResponseGate = CountDownLatch(if (delayAuthenticationResponse) 1 else 0)
        private val preferenceJob = SupervisorJob()
        private val preferenceScope = CoroutineScope(preferenceJob + Dispatchers.IO)
        private val preferences = HostPreferences(
            PreferenceDataStoreFactory.create(scope = preferenceScope) {
                File(root, "connection.preferences_pb")
            },
        )
        private val database = Room.inMemoryDatabaseBuilder(application, HolonDatabase::class.java).build()
        private val trace = TraceRecorder(File(root, "trace"))
        private val sessionStore = object : SessionCredentialStore {
            private var credential: String? = null
            override fun read(): String? = credential
            override fun write(credential: String) { this.credential = credential }
            override fun clear() { credential = null }
        }
        private val repository = HolonRepository(application, sessionStore, preferences, database.holonDao(), trace)
        private val models = ViewModelStore()
        lateinit var viewModel: HolonViewModel
            private set

        init {
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse =
                    if (request.method == "GET" && request.path == "/api/auth/method") {
                        check(authenticationResponseGate.await(10, TimeUnit.SECONDS))
                        MockResponse().setResponseCode(responseCode)
                            .setHeader("Content-Type", "application/json").setBody(body)
                    } else {
                        MockResponse().setResponseCode(404)
                            .setBody("""{"code":"not_found","message":"fixture route"}""")
                    }
            }
            server.start()
            instrumentation.runOnMainSync {
                viewModel = HolonViewModel(application, repository, trace)
                models.put("connection-authentication", viewModel)
            }
        }

        fun onMain(action: HolonViewModel.() -> Unit) =
            instrumentation.runOnMainSync { viewModel.action() }

        fun continueLogin(): Job {
            val initial = awaitState { it.phase == AppPhase.SignedOut }
            assertEquals("", initial.baseUrl)
            assertEquals("", initial.token)
            assertNull(initial.authenticationMethod)
            assertNull(initial.browserLoginUrl)
            lateinit var probe: Job
            val baseUrl = server.url("/api/").toString()
            onMain {
                assertNull(startOidcLogin())
                setBaseUrl(baseUrl)
                setAllowInsecureHttp(true)
                val existingJobs = viewModelScope.coroutineContext[Job]!!.children.toSet()
                this.continueLogin()
                probe = viewModelScope.coroutineContext[Job]!!.children.single { it !in existingJobs }
            }
            return probe
        }

        fun releaseAuthenticationResponse() = authenticationResponseGate.countDown()

        fun awaitProbeCompletion(probe: Job) = runBlocking {
            withTimeout(10_000) { probe.join() }
        }

        fun awaitState(predicate: (HolonUiState) -> Boolean): HolonUiState = runBlocking {
            withTimeout(10_000) { viewModel.state.first(predicate) }
        }

        fun assertAnonymousProbe() {
            val request = server.takeRequest(2, TimeUnit.SECONDS)
            assertNotNull(request)
            assertEquals("GET", request!!.method)
            assertEquals("/api/auth/method", request.path)
            assertNull(request.getHeader("Authorization"))
            assertNull(request.getHeader("Cookie"))
            assertEquals(1, server.requestCount)
        }

        fun assertRejectedAuthentication() {
            val rejected = awaitState { !it.busy && it.error != null }
            assertEquals(AppPhase.SignedOut, rejected.phase)
            assertNull(rejected.authenticationMethod)
            assertNull(rejected.browserLoginUrl)
            assertEquals("", rejected.token)
            assertFalse(rejected.showToken)
            assertNull(rejected.statusMessage)
            assertAnonymousProbe()
        }

        override fun close() {
            releaseAuthenticationResponse()
            instrumentation.runOnMainSync { models.clear() }
            runBlocking { preferenceJob.cancelAndJoin() }
            database.close()
            server.close()
            application.clearPreferences()
            check(root.deleteRecursively())
        }
    }

    private class FixtureApplication(context: Context, root: File) : Application() {
        private val fixtureFiles = File(root, "files").apply { check(mkdirs()) }
        private val fixtureCache = File(root, "cache").apply { check(mkdirs()) }
        private val preferencePrefix = "connection-auth-${root.name}-"
        private val preferenceNames = mutableSetOf<String>()

        init { attachBaseContext(context) }

        override fun getFilesDir(): File = fixtureFiles
        override fun getCacheDir(): File = fixtureCache

        override fun getSharedPreferences(name: String, mode: Int): SharedPreferences {
            val fixtureName = "$preferencePrefix$name"
            preferenceNames += fixtureName
            return super.getSharedPreferences(fixtureName, mode)
        }

        fun clearPreferences() {
            preferenceNames.forEach { baseContext.deleteSharedPreferences(it) }
        }
    }
}
