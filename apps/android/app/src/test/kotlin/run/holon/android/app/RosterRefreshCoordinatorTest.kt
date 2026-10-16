package run.holon.android.app

import java.net.ConnectException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import run.holon.android.sdk.HolonHttpException

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class RosterRefreshCoordinatorTest {
    @Test fun `manual and stream requests share one active read`() = runTest {
        val gate = CompletableDeferred<String>()
        var reads = 0
        val loaded = mutableListOf<String>()
        var refreshing = false
        val loader = RosterRefreshCoordinator(this, read = { reads++; gate.await() },
            onLoaded = loaded::add, onFailure = { throw it }, onRefreshing = { refreshing = it })
        loader.request()
        runCurrent()
        repeat(10) { loader.request(showProgress = true) }
        assertTrue(refreshing)
        assertEquals(1, reads)
        gate.complete("online")
        runCurrent()
        assertEquals(listOf("online"), loaded)
        assertFalse(refreshing)
    }

    @Test fun `transient snapshot failures recover without another hint`() = runTest {
        var attempts = 0
        val times = mutableListOf<Long>()
        val loaded = mutableListOf<String>()
        var refreshing = false
        val loader = RosterRefreshCoordinator(this, read = {
            times.add(testScheduler.currentTime)
            if (++attempts <= 6) throw HolonHttpException(503, null)
            "online"
        }, onLoaded = loaded::add, onFailure = {}, onRefreshing = { refreshing = it })
        loader.request(showProgress = true)
        runCurrent()
        assertFalse(refreshing)
        // Hints during backoff must not create extra reads or postpone recovery.
        repeat(50) { loader.request() }
        advanceTimeBy(61_000)
        runCurrent()
        assertEquals(listOf(0L, 1_000L, 3_000L, 7_000L, 15_000L, 31_000L, 61_000L), times)
        assertEquals(listOf("online"), loaded)
    }

    @Test fun `explicit retry wakes backoff and is still single flight`() = runTest {
        var reads = 0
        val loader = RosterRefreshCoordinator(this, read = {
            if (++reads == 1) throw ConnectException("offline")
            "online"
        }, onLoaded = {}, onFailure = {}, onRefreshing = {})
        loader.request()
        runCurrent()
        advanceTimeBy(100)
        repeat(10) { loader.request(showProgress = true) }
        runCurrent()
        assertEquals(2, reads)
        advanceTimeBy(60_000)
        runCurrent()
        assertEquals(2, reads)
    }

    @Test fun `permanent and authentication errors do not retry`() = runTest {
        for (status in listOf(401, 403, 400)) {
            var attempts = 0
            val failures = mutableListOf<Throwable>()
            val loader = RosterRefreshCoordinator(this, read = {
                attempts++
                throw HolonHttpException(status, null)
            }, onLoaded = {}, onFailure = failures::add, onRefreshing = {})
            loader.request()
            runCurrent()
            advanceTimeBy(60_000)
            runCurrent()
            assertEquals(1, attempts)
            assertEquals(status, (failures.single() as HolonHttpException).statusCode)
        }
    }

    @Test fun `reset on background cancels backoff and foreground can restart`() = runTest {
        var reads = 0
        val loader = RosterRefreshCoordinator(this, read = {
            reads++
            throw ConnectException("offline")
        }, onLoaded = {}, onFailure = {}, onRefreshing = {})
        loader.request()
        runCurrent()
        loader.reset()
        advanceTimeBy(60_000)
        runCurrent()
        assertEquals(1, reads)
        loader.request()
        runCurrent()
        assertEquals(2, reads)
        loader.reset()
    }

    @Test fun `reset fences a late response without clearing replacement progress`() = runTest {
        val oldGate = CompletableDeferred<String>()
        val newGate = CompletableDeferred<String>()
        val loaded = mutableListOf<String>()
        var reads = 0
        var refreshing = false
        val loader = RosterRefreshCoordinator(this,
            read = { withContext(NonCancellable) { if (++reads == 1) oldGate.await() else newGate.await() } },
            onLoaded = loaded::add, onFailure = { throw it }, onRefreshing = { refreshing = it })
        loader.request(showProgress = true)
        runCurrent()
        loader.reset()
        loader.request(showProgress = true)
        runCurrent()
        oldGate.complete("old user")
        runCurrent()
        assertTrue(refreshing)
        assertEquals(emptyList(), loaded)
        newGate.complete("new user")
        runCurrent()
        assertEquals(listOf("new user"), loaded)
        assertFalse(refreshing)
    }
}
