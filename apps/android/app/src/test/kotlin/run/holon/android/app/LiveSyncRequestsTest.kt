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
    @Test fun `permanent read failures are traced once per watermark until foreground reset`() = runTest {
        var attempts = 0
        val writer = BriefReadStateWriter(this, write = { _, _ -> attempts++; throw HolonHttpException(409, null) }, onFailure = {})
        writer.request("tester", 10)
        runCurrent()
        writer.request("tester", 10)
        runCurrent()
        assertEquals(1, attempts)
        writer.reset()
        writer.request("tester", 10)
        runCurrent()
        assertEquals(2, attempts)
    }

    @Test fun `preview reset discards late non cancellable response`() = runTest {
        val gate = CompletableDeferred<Unit>()
        val received = mutableListOf<String>()
        val loader = AgentPreviewLoader(this, read = { id: String -> withContext(NonCancellable) { gate.await(); id } },
            onLoaded = { _, value -> received.add(value) }, onFailure = { throw it })
        loader.request("old-user")
        advanceTimeBy(250)
        runCurrent()
        loader.reset()
        gate.complete(Unit)
        runCurrent()
        assertEquals(emptyList(), received)
    }

    @Test fun `read receipt retries lost response and coalesces newer read watermark`() = runTest {
        val writes = mutableListOf<Long>()
        var failures = 0
        val writer = BriefReadStateWriter(this, write = { _, through ->
            writes.add(through)
            if (writes.size == 1) throw ConnectException("offline")
        }, onFailure = { failures++ })
        writer.request("tester", 10)
        runCurrent()
        writer.request("tester", 12)
        writer.request("tester", 11)
        advanceTimeBy(1_000)
        runCurrent()
        assertEquals(listOf(10L, 12L), writes)
        assertEquals(1, failures)
        writer.request("tester", 12)
        runCurrent()
        assertEquals(2, writes.size)
    }

    @Test fun `read receipt reset stops retry and clears identity watermarks`() = runTest {
        var writes = 0
        var offline = true
        val writer = BriefReadStateWriter(this, write = { _, _ -> writes++; if (offline) throw ConnectException("offline") }, onFailure = {})
        writer.request("tester", 20)
        runCurrent()
        writer.reset()
        advanceTimeBy(5_000)
        runCurrent()
        assertEquals(1, writes)
        offline = false
        writer.request("tester", 1)
        runCurrent()
        assertEquals(2, writes)
    }

    @Test fun `preview reads bound concurrency and coalesce while requests run`() = runTest {
        val gate = CompletableDeferred<Unit>()
        var active = 0
        var maximum = 0
        var reads = 0
        val received = mutableListOf<String>()
        val loader = AgentPreviewLoader(this, read = { id: String ->
            active++
            maximum = maxOf(maximum, active)
            reads++
            gate.await()
            active--
            id
        }, onLoaded = { _, value -> received.add(value) }, onFailure = { throw it })
        repeat(12) { loader.request("agent-$it") }
        advanceTimeBy(250)
        runCurrent()
        repeat(5) { loader.request("agent-0") }
        assertEquals(4, maximum)
        gate.complete(Unit)
        advanceTimeBy(500)
        runCurrent()
        assertEquals(13, reads)
        assertEquals(13, received.size)
    }

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
