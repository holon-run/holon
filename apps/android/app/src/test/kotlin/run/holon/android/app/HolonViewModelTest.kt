package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.test.runTest
import run.holon.android.sdk.HolonApiError
import run.holon.android.sdk.HolonHttpException
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

    @Test
    fun `live sync callbacks are accepted only for the foreground current generation`() {
        assertTrue(isCurrentLiveSync(true, AppPhase.Ready, 3, 3))
        assertFalse(isCurrentLiveSync(false, AppPhase.Ready, 3, 3))
        assertFalse(isCurrentLiveSync(true, AppPhase.SignedOut, 3, 3))
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
}
