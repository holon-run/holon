package run.holon.android.app

import java.net.ConnectException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotSame
import kotlin.test.assertSame
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import run.holon.android.sdk.HolonHttpException
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
    fun `roster refresh coalesces across dispatchers and reset drops the old request`() = runTest {
        val firstGate = CompletableDeferred<Unit>()
        val replacementGate = CompletableDeferred<Unit>()
        var reads = 0
        var gate = firstGate
        val loader = RosterRefreshRequest(this) {
            reads++
            gate.await()
            "roster-$reads"
        }
        val firstResult = CompletableDeferred<kotlinx.coroutines.Deferred<String>>()
        val secondResult = CompletableDeferred<kotlinx.coroutines.Deferred<String>>()

        val firstJob = launch(Dispatchers.Default) { firstResult.complete(loader.load()) }
        val secondJob = launch(Dispatchers.IO) { secondResult.complete(loader.load()) }
        firstJob.join()
        secondJob.join()
        val first = firstResult.await()
        val second = secondResult.await()
        assertSame(first, second)
        runCurrent()
        assertEquals(1, reads)

        withContext(Dispatchers.Default) { loader.reset() }
        gate = replacementGate
        val replacement = withContext(Dispatchers.IO) { loader.load() }
        assertNotSame(first, replacement)
        runCurrent()
        assertEquals(2, reads)
        replacementGate.complete(Unit)
        assertEquals("roster-2", replacement.await())
    }

    @Test
    fun `authentication failure after transient failure is reported`() = runTest {
        var reads = 0
        val failures = mutableListOf<Throwable>()
        val loader =
            BriefReadStateLoader(
                scope = this,
                read = {
                    if (++reads == 1) {
                        throw HolonProtocolException("request failed", java.net.ConnectException())
                    }
                    throw HolonHttpException(statusCode = 401, apiError = null)
                },
                onLoaded = {},
                onFailure = failures::add,
            )

        loader.request()
        runCurrent()
        assertEquals(1, failures.size)
        advanceTimeBy(1_000)
        runCurrent()

        assertEquals(2, failures.size)
        assertEquals(401, (failures[1] as HolonHttpException).statusCode)
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

    @Test
    fun `brief read request and reset are safe across dispatchers`() = runTest {
        val gate = CompletableDeferred<Unit>()
        var old = true
        var reads = 0
        val loaded = mutableListOf<BriefReadSnapshot>()
        val loader =
            BriefReadStateLoader(
                scope = this,
                read = {
                    reads++
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

        launch(Dispatchers.Default) { loader.request() }.join()
        runCurrent()
        assertEquals(1, reads)

        launch(Dispatchers.IO) { loader.reset() }.join()
        old = false
        launch(Dispatchers.Default) { loader.request() }.join()
        runCurrent()
        assertEquals(2, reads)

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
