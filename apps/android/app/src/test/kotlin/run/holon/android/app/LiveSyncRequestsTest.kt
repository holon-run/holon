package run.holon.android.app

import java.net.ConnectException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertSame
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import run.holon.android.sdk.HolonProtocolException

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class LiveSyncRequestsTest {
    @Test
    fun `roster refresh shares one active request`() = runTest {
        val gate = CompletableDeferred<Unit>()
        var reads = 0
        val loader = RosterRefreshRequest(this) {
            reads++
            gate.await()
            "roster"
        }

        val first = loader.load()
        val second = loader.load()
        assertSame(first, second)
        runCurrent()
        assertEquals(1, reads)
        gate.complete(Unit)
        assertEquals("roster", first.await())
    }

    @Test
    fun `brief read refresh coalesces and retries transient failures`() = runTest {
        var reads = 0
        val loaded = mutableListOf<BriefReadSnapshot>()
        val failures = mutableListOf<Throwable>()
        val loader =
            BriefReadStateLoader(
                scope = this,
                read = {
                    if (++reads == 1) {
                        throw HolonProtocolException("request failed", ConnectException())
                    }
                    BriefReadSnapshot.Legacy(mapOf("agent-1" to "brief-1"))
                },
                onLoaded = loaded::add,
                onFailure = failures::add,
            )

        loader.request()
        loader.request()
        runCurrent()
        assertEquals(1, failures.size)
        advanceTimeBy(1_000)
        runCurrent()

        assertEquals(2, reads)
        assertEquals(1, loaded.size)
        assertEquals(1, failures.size)
    }

    @Test
    fun `brief read refresh ignores a noncancellable result after reset`() = runTest {
        val gate = CompletableDeferred<Unit>()
        var old = true
        val loaded = mutableListOf<BriefReadSnapshot>()
        val loader =
            BriefReadStateLoader(
                scope = this,
                read = {
                    if (old) {
                        withContext(NonCancellable) {
                            gate.await()
                            BriefReadSnapshot.Legacy(mapOf("agent-1" to "old"))
                        }
                    } else {
                        BriefReadSnapshot.Legacy(mapOf("agent-1" to "new"))
                    }
                },
                onLoaded = loaded::add,
                onFailure = {},
            )

        loader.request()
        runCurrent()
        loader.reset()
        old = false
        loader.request()
        runCurrent()
        gate.complete(Unit)
        runCurrent()

        assertEquals(
            listOf<BriefReadSnapshot>(
                BriefReadSnapshot.Legacy(mapOf("agent-1" to "new")),
            ),
            loaded,
        )
    }
}
