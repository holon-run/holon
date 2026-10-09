package run.holon.android.app

import java.net.ConnectException
import java.net.UnknownHostException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.runCurrent
import run.holon.android.sdk.HolonApiError
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonProtocolException
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonServerInfo
import run.holon.android.sdk.HolonWorkspace

class HolonViewModelTest {
    private val workspace =
        HolonWorkspace(
            workspaceId = "workspace-1",
            alias = "main",
            label = "Main",
            isActive = true,
            executionRootId = "root-1",
            projectionKind = "git_worktree_root",
        )

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `new session transition waits for expired session cleanup`() = runTest {
        val barrier = SessionResetBarrier(this)
        val release = CompletableDeferred<Unit>()
        var resetCompleted = false
        var loginStarted = false

        barrier.schedule {
            release.await()
            resetCompleted = true
        }
        val login = launch {
            barrier.await()
            loginStarted = true
        }

        runCurrent()
        assertFalse(resetCompleted)
        assertFalse(loginStarted)

        release.complete(Unit)
        login.join()
        assertTrue(resetCompleted)
        assertTrue(loginStarted)
    }

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `failed session cleanup does not block a new session transition`() = runTest {
        val barrier = SessionResetBarrier(this)
        val login = CompletableDeferred<Unit>()

        barrier.schedule { error("expired session cleanup failed") }
        launch {
            barrier.await()
            login.complete(Unit)
        }

        runCurrent()
        assertTrue(login.isCompleted)
        login.await()
    }

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `session cleanup scheduled during a new transition is ignored`() = runTest {
        val barrier = SessionResetBarrier(this)
        var resetRan = false

        barrier.beginTransition()
        barrier.schedule { resetRan = true }
        runCurrent()

        assertFalse(resetRan)
        barrier.endTransition()
    }

    @Test
    fun `adding a network preserves the current session until a new login succeeds`() {
        val session =
            ActiveSession(
                networkId = "network-a",
                baseUrl = "https://office.example/api/",
                user = HolonCurrentUser("user-a", "Alice", "session"),
                runtimeId = "runtime-a",
                visibilityScopeId = "scope-a",
                server = HolonServerInfo("", "local", true, emptySet()),
            )
        val profile = NetworkProfile("network-a", "Office", session.baseUrl, false)
        val current =
            HolonUiState(
                phase = AppPhase.Ready,
                baseUrl = session.baseUrl,
                session = session,
                networkProfiles = listOf(profile),
                mainDestination = MainDestination.Settings,
            )

        val adding = current.forAddingNetwork()
        assertEquals(AppPhase.AddingNetwork, adding.phase)
        assertEquals("", adding.baseUrl)
        assertEquals(session, adding.session)
        assertEquals(listOf(profile), adding.networkProfiles)

        val restored = adding.copy(baseUrl = "http://lab.example:7878", token = "temporary").afterCancelAddingNetwork()
        assertEquals(AppPhase.Ready, restored.phase)
        assertEquals(session.baseUrl, restored.baseUrl)
        assertEquals("", restored.token)
        assertEquals(session, restored.session)
        assertEquals(listOf(profile), restored.networkProfiles)
        assertEquals(MainDestination.Settings, restored.mainDestination)
    }

    @Test
    fun `deleting another network preserves the active session and screen`() {
        val current = networkDeletionState()
        val remaining = current.networkProfiles.filterNot { it.networkId == "network-b" }

        val updated = current.copy(busy = true).afterDeletingNetwork("network-b", remaining)

        assertEquals(current.session, updated.session)
        assertEquals(current.phase, updated.phase)
        assertEquals(current.mainDestination, updated.mainDestination)
        assertEquals(current.draft, updated.draft)
        assertEquals(current.token, updated.token)
        assertEquals(remaining, updated.networkProfiles)
        assertFalse(updated.busy)
    }

    @Test
    fun `deleting the current network clears session data without switching to remaining network`() {
        val current = networkDeletionState()
        val remaining = current.networkProfiles.filterNot { it.networkId == "network-a" }

        val updated = current.afterDeletingNetwork("network-a", remaining)

        assertEquals(AppPhase.SignedOut, updated.phase)
        assertEquals(null, updated.session)
        assertEquals("", updated.baseUrl)
        assertEquals("", updated.token)
        assertEquals("", updated.draft)
        assertTrue(updated.agents.isEmpty())
        assertEquals(MainDestination.Agents, updated.mainDestination)
        assertEquals(remaining, updated.networkProfiles)
        assertFalse(updated.busy)
    }

    @Test
    fun `deleting the last network leaves an empty signed out screen`() {
        val current = networkDeletionState()
        val updated = current.afterDeletingNetwork("network-a", emptyList())

        assertEquals(AppPhase.SignedOut, updated.phase)
        assertEquals(null, updated.session)
        assertTrue(updated.networkProfiles.isEmpty())
        assertFalse(shouldShowSavedNetworks(false, updated.networkProfiles))
    }

    @Test
    fun `deleting the saved login target clears its form credentials`() {
        val current = networkDeletionState().copy(phase = AppPhase.SignedOut, session = null)
        val updated = current.afterDeletingNetwork("network-a", current.networkProfiles.drop(1))

        assertEquals("", updated.baseUrl)
        assertEquals("", updated.token)
        assertFalse(updated.showToken)
        assertEquals(AppPhase.SignedOut, updated.phase)
    }

    @Test
    fun `network deletion rejects unknown targets and conflicting operations`() {
        val current = networkDeletionState()
        assertTrue(current.canDeleteNetwork("network-a"))
        assertTrue(current.copy(phase = AppPhase.SignedOut, session = null).canDeleteNetwork("network-b"))
        assertFalse(current.canDeleteNetwork("unknown"))
        assertFalse(current.copy(busy = true).canDeleteNetwork("network-a"))
        assertFalse(current.copy(enqueueing = true).canDeleteNetwork("network-a"))
        assertFalse(current.copy(stagingAttachment = true).canDeleteNetwork("network-a"))
        assertFalse(current.copy(phase = AppPhase.AddingNetwork).canDeleteNetwork("network-a"))
        assertFalse(current.copy(phase = AppPhase.Starting).canDeleteNetwork("network-a"))
    }

    private fun networkDeletionState(): HolonUiState {
        val session = ActiveSession(
            "network-a", "https://office.example/api/",
            HolonCurrentUser("user-a", "Alice", "session"), "runtime-a", "scope-a",
            HolonServerInfo("", "local", true, emptySet()),
        )
        return HolonUiState(
            phase = AppPhase.Ready,
            baseUrl = session.baseUrl,
            session = session,
            networkProfiles = listOf(
                NetworkProfile("network-a", "Office", session.baseUrl, false),
                NetworkProfile("network-b", "Lab", "https://lab.example/api/", false),
            ),
            mainDestination = MainDestination.Settings,
            draft = "Unsent draft",
            token = "Form token",
            showToken = true,
        )
    }

    @Test
    fun `live sync callbacks are accepted only for the foreground current generation`() {
        assertTrue(isCurrentLiveSync(true, AppPhase.Ready, 3, 3))
        assertFalse(isCurrentLiveSync(false, AppPhase.Ready, 3, 3))
        assertFalse(isCurrentLiveSync(true, AppPhase.SignedOut, 3, 3))
        assertFalse(isCurrentLiveSync(true, AppPhase.AddingNetwork, 3, 3))
        assertFalse(isCurrentLiveSync(true, AppPhase.Ready, 2, 3))
    }

    @Test
    fun `stale workspace browse request cannot update selected workspace`() {
        val oldRequest = WorkspaceBrowseRequest(1, workspace.workspaceId, workspace.executionRootId, "")
        val currentRequest = WorkspaceBrowseRequest(2, workspace.workspaceId, workspace.executionRootId, "apps")

        assertFalse(oldRequest.appliesTo(currentRequest, workspace))
        assertTrue(currentRequest.appliesTo(currentRequest, workspace))
    }

    @Test
    fun `workspace browse request requires the selected workspace`() {
        val request = WorkspaceBrowseRequest(1, workspace.workspaceId, workspace.executionRootId, "")
        val otherWorkspace = workspace.copy(workspaceId = "workspace-2")

        assertFalse(request.appliesTo(request, otherWorkspace))
        assertFalse(request.appliesTo(request, null))
    }

    @Test
    fun `workspace browse cancellation is propagated`() = runTest {
        val error =
            assertFailsWith<CancellationException> {
                executeWorkspaceBrowseRequest<Int> {
                    throw CancellationException("superseded")
                }
            }

        assertEquals("superseded", error.message)
    }

    @Test
    fun `workspace browse failure remains available to the current request`() = runTest {
        val result =
            executeWorkspaceBrowseRequest<Int> {
                error("daemon unavailable")
            }

        assertTrue(result.isFailure)
        assertEquals("daemon unavailable", result.exceptionOrNull()?.message)
    }

    @Test
    fun `stale event cursor error is classified for restart`() {
        val error =
            HolonHttpException(
                statusCode = 404,
                apiError =
                    HolonApiError(
                        code = "cursor_not_found",
                        message = "cursor expired",
                        retryable = false,
                        detail = null,
                        domain = null,
                        context = emptyMap(),
                    ),
            )

        assertTrue(error.isStaleAgentEventCursor())
        assertFalse(
            HolonHttpException(
                    statusCode = 404,
                    apiError = error.apiError?.copy(code = "not_found"),
                )
                .isStaleAgentEventCursor(),
        )
    }

    @Test
    fun `only explicit authentication responses require login`() {
        assertTrue(
            HolonHttpException(
                statusCode = 401,
                apiError = null,
            ).isAuthenticationFailure(),
        )
        assertFalse(
            HolonHttpException(
                statusCode = 403,
                apiError = null,
            ).isAuthenticationFailure(),
        )
        assertTrue(
            HolonHttpException(
                statusCode = 401,
                apiError =
                    HolonApiError(
                        code = "session_expired_or_revoked",
                        message = "session is expired or revoked",
                        retryable = false,
                        detail = null,
                        domain = null,
                        context = emptyMap(),
                    ),
            ).isAuthenticationFailure(),
        )
        assertFalse(
            HolonHttpException(
                statusCode = 401,
                apiError =
                    HolonApiError(
                        code = "forbidden",
                        message = "not allowed",
                        retryable = false,
                        detail = null,
                        domain = null,
                        context = emptyMap(),
                    ),
            ).isAuthenticationFailure(),
        )
        assertFalse(
            HolonHttpException(
                statusCode = 503,
                apiError = null,
            ).isAuthenticationFailure(),
        )
    }

    @Test
    fun `transport and transient server failures stay recoverable`() {
        assertTrue(HolonProtocolException("request failed", ConnectException()).isTransientNetworkFailure())
        assertTrue(HolonProtocolException("request failed", UnknownHostException()).isTransientNetworkFailure())
        assertTrue(
            HolonHttpException(
                statusCode = 503,
                apiError = null,
            ).isTransientNetworkFailure(),
        )
        assertFalse(
            HolonHttpException(
                statusCode = 400,
                apiError = null,
            ).isTransientNetworkFailure(),
        )
    }

    @Test
    fun `unread count errors have one localized prefix`() {
        assertEquals(
            "Could not load unread counts: Cannot connect to Holon. Check the network and address.",
            UiCopy.translate("无法加载未读数：无法连接 Holon，请检查网络和地址", "en"),
        )
    }

    @Test
    fun `transient unread count failure keeps the cached read state`() {
        val cached =
            run.holon.android.sdk.HolonBriefReadState(
                agentId = "agent-1",
                eventHeadSeq = 12,
                eventLogEpoch = "epoch-1",
                oldestRetainedSeq = 1,
                readThroughEventSeq = 4,
                resetRequired = false,
                retentionGap = false,
                revision = 2,
                unreadCount = 3,
                visibilityScopeId = "scope-1",
            )
        val current =
            HolonUiState(
                phase = AppPhase.Ready,
                online = true,
                briefReadStates = mapOf("agent-1" to cached),
                briefReadStatesLoaded = true,
            )

        val afterTransientFailure =
            current.copy(
                online = false,
                error = null,
                statusMessage = TRANSIENT_NETWORK_STATUS_MESSAGE,
            )

        assertEquals(current.briefReadStates, afterTransientFailure.briefReadStates)
        assertTrue(afterTransientFailure.briefReadStatesLoaded)
        assertEquals(null, afterTransientFailure.error)
    }
}
