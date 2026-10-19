package run.holon.android.app

import android.app.Application
import android.content.Context
import android.content.SharedPreferences
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.lifecycle.ViewModelStore
import androidx.room.Room
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
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
import run.holon.android.sdk.ProfileSessionCredentialStore

/** Real Room + HTTP + ViewModel; preferences, credentials and files are fixture-owned. */
@RunWith(AndroidJUnit4::class)
class NetworkCacheFirstIntegrationTest {
    @Test
    fun targetCachePrecedesHttpAndNeverOverwritesFreshSnapshotAcrossRepeatedSwitches() {
        Fixture().use { fixture ->
            for (target in listOf(fixture.a, fixture.b, fixture.a)) {
                target.blockCurrentUser()
                val profile = fixture.profile(target)
                fixture.onMain { switchNetwork(profile.networkId) }
                val cached = fixture.awaitState { it.session?.networkId == profile.networkId && it.busy }
                assertTrue(target.userRequested.await(5, TimeUnit.SECONDS))
                assertEquals(listOf(target.agentId), cached.agents.map { it.id })
                assertEquals(AgentRosterState.Cached, cached.rosterState)
                assertFalse(cached.online)
                assertTrue(cached.session!!.isOffline)
                assertNull(cached.lastSyncedAt)
                assertTrue(cached.operatorPreviews.isEmpty())
                assertFalse(cached.briefReadStatesLoaded)

                val requests = target.userRequests.get()
                fixture.onMain { refresh() }
                assertEquals(requests, target.userRequests.get())
                target.agentName = "Fresh ${target.name}"
                target.release()
                val online = fixture.awaitState { !it.busy && it.session?.networkId == profile.networkId }
                assertEquals(AgentRosterState.Synced, online.rosterState)
                assertTrue(online.online)
                assertFalse(online.session!!.isOffline)
                assertEquals("Fresh ${target.name}", online.agents.single().displayName)
                assertNotNull(online.lastSyncedAt)
            }
        }
    }

    @Test
    fun transientFailureKeepsTargetCacheAndRetryRecoversOnline() {
        Fixture().use { fixture ->
            fixture.a.blockCurrentUser()
            fixture.a.userStatus = 503
            fixture.onMain { switchNetwork(fixture.aProfile.networkId) }
            fixture.awaitState { it.session?.networkId == fixture.aProfile.networkId && it.busy }
            fixture.a.release()
            val offline = fixture.awaitState { !it.busy }
            assertEquals(listOf(fixture.a.agentId), offline.agents.map { it.id })
            assertEquals(AgentRosterState.Cached, offline.rosterState)
            assertFalse(offline.online)
            fixture.a.userStatus = 200
            fixture.onMain { onForeground() }
            val recovered = fixture.awaitState { it.online && it.session?.networkId == fixture.aProfile.networkId }
            assertEquals(AgentRosterState.Synced, recovered.rosterState)
            assertFalse(recovered.session!!.isOffline)
        }
    }

    @Test
    fun noCacheRemainsUnknownUntilServerConfirmsEmptyRoster() {
        Fixture().use { fixture ->
            runBlocking { fixture.database.holonDao().clearScope(fixture.aProfile.savedScopeKey()!!) }
            fixture.a.emptyRoster = true
            fixture.a.blockCurrentUser()
            fixture.onMain { switchNetwork(fixture.aProfile.networkId) }
            val loading = fixture.awaitState { it.session?.networkId == fixture.aProfile.networkId && it.busy }
            assertEquals(AgentRosterState.NotLoaded, loading.rosterState)
            assertTrue(loading.agents.isEmpty())
            assertFalse(loading.online)
            fixture.a.release()
            val empty = fixture.awaitState { !it.busy }
            assertEquals(AgentRosterState.Synced, empty.rosterState)
            assertTrue(empty.agents.isEmpty())
            assertTrue(empty.online)
        }
    }

    @Test
    fun authenticationFailureRemovesThePreviouslyDisplayedTargetCache() {
        Fixture().use { fixture ->
            fixture.a.blockCurrentUser()
            fixture.a.userStatus = 401
            fixture.onMain { switchNetwork(fixture.aProfile.networkId) }
            fixture.awaitState { it.rosterState == AgentRosterState.Cached }
            fixture.a.release()
            val rejected = fixture.awaitState { it.phase == AppPhase.SignedOut }
            assertTrue(rejected.agents.isEmpty())
            assertEquals(AgentRosterState.NotLoaded, rejected.rosterState)
            assertNull(rejected.session)
            assertTrue(runBlocking { fixture.database.holonDao().conversations(fixture.aProfile.savedScopeKey()!!).isEmpty() })
        }
    }

    @Test
    fun changedVisibilityScopeReplacesCacheAndClearsTheOldProjection() {
        Fixture().use { fixture ->
            fixture.a.blockCurrentUser()
            fixture.a.visibility = "changed-visibility"
            fixture.onMain { switchNetwork(fixture.aProfile.networkId) }
            val cached = fixture.awaitState { it.rosterState == AgentRosterState.Cached }
            assertEquals(fixture.aProfile.visibilityScopeId, cached.session!!.visibilityScopeId)
            fixture.a.release()
            val changed = fixture.awaitState { !it.busy }
            assertEquals("changed-visibility", changed.session!!.visibilityScopeId)
            assertEquals(AgentRosterState.Synced, changed.rosterState)
            assertTrue(runBlocking { fixture.database.holonDao().conversations(fixture.aProfile.savedScopeKey()!!).isEmpty() })
        }
    }

    @Test
    fun missingCredentialNeverPublishesSavedProjectionOrRequestsHttp() {
        Fixture().use { fixture ->
            fixture.credentials.clear(fixture.aProfile.networkId)
            val requests = fixture.a.userRequests.get()
            fixture.onMain { switchNetwork(fixture.aProfile.networkId) }
            val rejected = fixture.awaitState { it.phase == AppPhase.SignedOut }
            assertTrue(rejected.agents.isEmpty())
            assertNull(rejected.session)
            assertEquals(requests, fixture.a.userRequests.get())
        }
    }

    private class Fixture : AutoCloseable {
        private val instrumentation = InstrumentationRegistry.getInstrumentation()
        private val root = File(instrumentation.targetContext.cacheDir, "network-cache-${UUID.randomUUID()}")
            .apply { check(mkdirs()) }
        private val application = FixtureApplication(instrumentation.targetContext, root)
        private val preferenceJob = SupervisorJob()
        private val preferences = HostPreferences(PreferenceDataStoreFactory.create(
            scope = CoroutineScope(preferenceJob + Dispatchers.IO),
        ) { File(root, "network.preferences_pb") })
        val database = Room.inMemoryDatabaseBuilder(application, HolonDatabase::class.java).build()
        private val trace = TraceRecorder(File(root, "trace"))
        val credentials = object : ProfileSessionCredentialStore {
            private val values = ConcurrentHashMap<String, String>()
            override fun read(): String? = null
            override fun write(credential: String) = error("Unscoped credential write")
            override fun clear() = values.clear()
            override fun read(profileId: String): String? = values[profileId]
            override fun write(profileId: String, credential: String) { values[profileId] = credential }
            override fun clear(profileId: String) { values.remove(profileId) }
        }
        private val repository = HolonRepository(application, credentials, preferences, database.holonDao(), trace)
        val a = Host("a")
        val b = Host("b")
        val aProfile: NetworkProfile
        private val models = ViewModelStore()
        private val viewModel: HolonViewModel

        init {
            runBlocking {
                for (host in listOf(a, b)) {
                    repository.login(host.server.url("/api/").toString(), "fixture".toCharArray(), true)
                }
            }
            aProfile = profile(a)
            lateinit var created: HolonViewModel
            instrumentation.runOnMainSync {
                created = HolonViewModel(application, repository, trace)
                models.put("network-cache", created)
                created.onBackground()
            }
            viewModel = created
            awaitState { it.phase == AppPhase.Ready && it.online }
        }

        fun profile(host: Host): NetworkProfile = runBlocking {
            repository.networkProfiles().single { it.baseUrl == host.server.url("/api/").toString() }
        }

        fun onMain(action: HolonViewModel.() -> Unit) =
            instrumentation.runOnMainSync { viewModel.action() }

        fun awaitState(predicate: (HolonUiState) -> Boolean): HolonUiState = runBlocking {
            withTimeout(10_000) { viewModel.state.first(predicate) }
        }

        override fun close() {
            a.release()
            b.release()
            instrumentation.runOnMainSync { models.clear() }
            runBlocking { preferenceJob.cancelAndJoin() }
            database.close()
            a.server.close()
            b.server.close()
            application.clearPreferences()
            check(root.deleteRecursively())
        }
    }

    private class Host(val name: String) {
        val server = MockWebServer()
        val agentId = "agent-$name"
        val userRequests = AtomicInteger()
        @Volatile var userRequested = CountDownLatch(0)
        @Volatile private var userGate = CountDownLatch(0)
        @Volatile var userStatus = 200
        @Volatile var emptyRoster = false
        @Volatile var visibility = "visibility-$name"
        @Volatile var agentName = "Cached $name"

        init {
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path.orEmpty().substringBefore('?')
                    if (path.endsWith("/auth/session/me")) {
                        userRequests.incrementAndGet()
                        userRequested.countDown()
                        check(userGate.await(30, TimeUnit.SECONDS))
                        if (userStatus != 200) return MockResponse().setResponseCode(userStatus).setBody("{}")
                    }
                    val body = when {
                        path.endsWith("/auth/session/exchange/native") ->
                            """{"ok":true,"credential":"fixture-$name","user_id":"user-$name","expires_at":null}"""
                        path.endsWith("/auth/session/me") ->
                            """{"ok":true,"user_id":"user-$name","display_name":"Fixture $name","auth_method":"local"}"""
                        path.endsWith("/handshake") ->
                            """{"ok":true,"protocol":{"name":"holon-control","version":1},"auth":{"mode":"bearer","required":true},"runtime":{"default_agent":"$agentId","workspace_dir":"/test","home_dir":"/test","listen":"127.0.0.1:0"},"capabilities":["agents.conversation-read.v1","auth.native-session.v1","control.prompt-idempotency.v1","control.prompt-attachments.v1","brief.attachments.v1"]}"""
                        path.endsWith("/agents/snapshot") -> {
                            val agents = if (emptyRoster) "" else
                                """{"agent":{"identity":{"agent_id":"$agentId","name":"$agentName","is_default_agent":true,"status":"active"},"model":{"effective_model":"test/model","runtime_default_model":"test/model","source":"runtime_default"},"status":"awake_idle","pending":0}}"""
                            """{"runtime_id":"runtime-$name","event_log_epoch":"epoch-$name","visibility_scope_id":"$visibility","agents":[$agents]}"""
                        }
                        path.endsWith("/brief-read-states") -> "[]"
                        path.endsWith("/stream") -> return MockResponse().setHeader("Content-Type", "text/event-stream").setBody(": fixture\n\n")
                        else -> return MockResponse().setResponseCode(404).setBody("{}")
                    }
                    return MockResponse().setHeader("Content-Type", "application/json").setBody(body)
                }
            }
            server.start()
        }

        fun blockCurrentUser() {
            userRequested = CountDownLatch(1)
            userGate = CountDownLatch(1)
        }

        fun release() = userGate.countDown()
    }

    private class FixtureApplication(context: Context, root: File) : Application() {
        private val fixtureFiles = File(root, "files").apply { check(mkdirs()) }
        private val fixtureCache = File(root, "cache").apply { check(mkdirs()) }
        private val preferenceNames = mutableSetOf<String>()
        private val prefix = root.name
        init { attachBaseContext(context) }
        override fun getFilesDir(): File = fixtureFiles
        override fun getCacheDir(): File = fixtureCache
        override fun getSharedPreferences(name: String, mode: Int): SharedPreferences {
            val fixtureName = "$prefix-$name"
            preferenceNames += fixtureName
            return super.getSharedPreferences(fixtureName, mode)
        }
        fun clearPreferences() {
            preferenceNames.forEach { baseContext.deleteSharedPreferences(it) }
        }
    }
}
