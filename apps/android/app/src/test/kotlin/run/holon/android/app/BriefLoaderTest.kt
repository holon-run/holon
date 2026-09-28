package run.holon.android.app

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class BriefLoaderTest {
    @Test fun `visible history has no twenty result cap and duplicate reads are bounded`() = runTest {
        val gate = CompletableDeferred<Unit>()
        var active = 0
        var maximum = 0
        val loaded = mutableListOf<String>()
        val loader = BriefLoader(this, read = { id: String ->
            active++
            maximum = maxOf(maximum, active)
            gate.await()
            active--
            id
        }, onLoading = {}, onLoaded = { id, _ -> loaded.add(id) }, onFailure = { _, error -> throw error })
        val ids = (1..35).map(Int::toString)
        loader.request(ids)
        loader.request(ids)
        runCurrent()
        assertEquals(3, maximum)
        gate.complete(Unit)
        runCurrent()
        assertEquals(ids, loaded)
    }

    @Test fun `failure is explicit and only retried on request`() = runTest {
        var reads = 0
        val failed = mutableListOf<String>()
        val loaded = mutableListOf<String>()
        val loader = BriefLoader(this, read = { id: String -> if (++reads == 1) error("offline") else id }, onLoading = {}, onLoaded = { id, _ -> loaded.add(id) }, onFailure = { id, _ -> failed.add(id) })
        loader.request(listOf("a"))
        runCurrent()
        loader.request(listOf("a"))
        runCurrent()
        assertEquals(1, reads)
        assertEquals(listOf("a"), failed)
        loader.request(listOf("a"), retry = true)
        runCurrent()
        assertEquals(listOf("a"), loaded)
    }

    @Test fun `identity reset ignores even noncancellable old results and permits same id again`() = runTest {
        val gate = CompletableDeferred<Unit>()
        var old = true
        val loaded = mutableListOf<String>()
        val errors = mutableListOf<Throwable>()
        val loader = BriefLoader(this, read = { _: String ->
            if (old) withContext(NonCancellable) { gate.await(); "old-runtime" } else "new-runtime"
        }, onLoading = {}, onLoaded = { _, value -> loaded.add(value) }, onFailure = { _, error -> errors.add(error) })
        loader.request(listOf("same-id", "queued"))
        runCurrent()
        loader.reset()
        old = false
        loader.request(listOf("same-id"))
        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf("new-runtime"), loaded)
        assertTrue(errors.isEmpty())
    }
}
