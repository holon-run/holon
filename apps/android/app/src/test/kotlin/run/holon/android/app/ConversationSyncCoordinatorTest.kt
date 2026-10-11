package run.holon.android.app

import java.net.ConnectException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import run.holon.android.sdk.HolonApiError
import run.holon.android.sdk.HolonHttpException

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class ConversationSyncCoordinatorTest {
    @Test
    fun `409 resets cursor and refreshes snapshot before reopening stream`() = runTest {
        val streamCursors = mutableListOf<String?>()
        val freshRequests = mutableListOf<Boolean>()
        val snapshots = mutableListOf<String>()
        val states = mutableListOf<ConversationSyncState>()
        val streamGate = CompletableDeferred<Unit>()
        val coordinator =
            ConversationSyncCoordinator<String>(
                scope = this,
                isCurrent = { true },
                stream = { cursor, _ ->
                    streamCursors += cursor
                    if (streamCursors.size == 1) {
                        throw HolonHttpException(409, null)
                    }
                    streamGate.await()
                },
                snapshot = { fresh ->
                    freshRequests += fresh
                    "fresh-cursor"
                },
                cursorOf = { it },
                onSnapshot = { snapshot, fresh, live ->
                    snapshots += "$snapshot:$fresh:$live"
                },
                onState = states::add,
                onFailure = { throw it },
                closeStream = {},
            )

        coordinator.start("stale-cursor")
        runCurrent()

        assertEquals(listOf<String?>("stale-cursor", "fresh-cursor"), streamCursors)
        assertEquals(listOf(true), freshRequests)
        assertEquals(listOf("fresh-cursor:true:false"), snapshots)
        assertTrue(ConversationSyncState.NeedsRefresh in states)
        assertTrue(ConversationSyncState.Cached in states)

        coordinator.stop()
        runCurrent()
    }

    @Test
    fun `transient stream failure keeps cursor while snapshot fallback supplies cached state`() = runTest {
        val streamCursors = mutableListOf<String?>()
        val freshRequests = mutableListOf<Boolean>()
        val snapshots = mutableListOf<String>()
        val streamGate = CompletableDeferred<Unit>()
        val coordinator =
            ConversationSyncCoordinator<String>(
                scope = this,
                isCurrent = { true },
                stream = { cursor, _ ->
                    streamCursors += cursor
                    if (streamCursors.size == 1) {
                        throw ConnectException("offline")
                    }
                    streamGate.await()
                },
                snapshot = { fresh ->
                    freshRequests += fresh
                    "cached-cursor"
                },
                cursorOf = { it },
                onSnapshot = { snapshot, fresh, live ->
                    snapshots += "$snapshot:$fresh:$live"
                },
                onState = {},
                onFailure = { throw it },
                closeStream = {},
            )

        coordinator.start("last-cursor")
        runCurrent()
        advanceTimeBy(2_000)
        runCurrent()

        assertEquals(listOf<String?>("last-cursor", "last-cursor"), streamCursors)
        assertEquals(listOf(false), freshRequests)
        assertEquals(listOf("cached-cursor:false:false"), snapshots)

        coordinator.stop()
        runCurrent()
    }

    @Test
    fun `only conversation reset conflicts are classified for cursor recovery`() {
        assertTrue(HolonHttpException(409, null).isConversationHistoryResetRequired())
        assertTrue(
            HolonHttpException(
                409,
                HolonApiError(
                    code = "conversation_reset_required",
                    message = "reset",
                    retryable = false,
                    detail = null,
                    domain = null,
                    context = emptyMap(),
                ),
            ).isConversationHistoryResetRequired(),
        )
        assertFalse(
            HolonHttpException(
                409,
                HolonApiError(
                    code = "other_conflict",
                    message = "conflict",
                    retryable = false,
                    detail = null,
                    domain = null,
                    context = emptyMap(),
                ),
            ).isConversationHistoryResetRequired(),
        )
    }
}
