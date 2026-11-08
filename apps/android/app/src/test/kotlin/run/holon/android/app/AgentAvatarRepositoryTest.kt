package run.holon.android.app

import java.io.IOException
import java.nio.ByteBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.runCurrent
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import run.holon.android.sdk.HolonAvatarExpression
import run.holon.android.sdk.HolonHttpException

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class AgentAvatarRepositoryTest {
    private fun key(id: String = "agent", expression: HolonAvatarExpression = HolonAvatarExpression.Neutral) =
        AgentAvatarKey(id, expression, 64)

    private fun png(pixels: Int = 64): ByteArray =
        ByteBuffer.allocate(33).put(byteArrayOf(0x89.toByte(), 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a))
            .putInt(13).putInt(0x49484452).putInt(pixels).putInt(pixels).array()

    @Test fun `requests coalesce and offline reads never fetch uncached expressions`() = runTest {
        var calls = 0
        val repository = AgentAvatarRepository(
            read = { calls++; png() }, isCurrent = { true }, onAuthenticationFailure = {},
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        try {
            assertNull(repository.image(key(), allowNetwork = false))
            val first = async { repository.image(key(), true) }
            val second = async { repository.image(key(), true) }
            assertContentEquals(first.await(), second.await())
            assertNotNull(repository.image(key(), false))
            assertNull(repository.image(key(expression = HolonAvatarExpression.Focused), false))
            assertEquals(1, calls)
        } finally { repository.close() }
    }

    @Test fun `LRU bounds both bytes and entry count`() = runTest {
        for ((entries, bytes) in listOf(2 to 1024, 128 to 66)) {
            val reads = mutableListOf<String>()
            val repository = AgentAvatarRepository(
                read = { reads += it.agentId; png() }, isCurrent = { true }, onAuthenticationFailure = {},
                dispatcher = StandardTestDispatcher(testScheduler), maxEntries = entries, maxBytes = bytes,
            )
            try {
                repository.image(key("A"), true)
                repository.image(key("B"), true)
                repository.image(key("A"), true)
                repository.image(key("C"), true)
                assertNull(repository.image(key("B"), false))
                assertNotNull(repository.image(key("A"), false))
                repository.image(key("B"), true)
                assertEquals(listOf("A", "B", "C", "B"), reads)
            } finally { repository.close() }
        }
    }

    @Test fun `404 and bad images cool down without disabling other agents or later retries`() = runTest {
        for (error in listOf(IOException("offline"), HolonHttpException(404, null))) {
            var calls = 0
            val repository = AgentAvatarRepository(
                read = { if (++calls == 1) throw error else png() },
                isCurrent = { true }, onAuthenticationFailure = {},
                dispatcher = StandardTestDispatcher(testScheduler), nowMillis = { testScheduler.currentTime },
            )
            try {
                assertNull(repository.image(key(), true))
                assertNull(repository.image(key(), true))
                assertNotNull(repository.image(key("other"), true))
                advanceTimeBy(30_001)
                assertNotNull(repository.image(key(), true))
                assertEquals(3, calls)
                repository.reject(key())
                assertNull(repository.image(key(), false))
                assertNull(repository.image(key(), true))
            } finally { repository.close() }
        }
    }

    @Test fun `503 retries once with server delay and then cools down`() = runTest {
        var calls = 0
        val repository = AgentAvatarRepository(
            read = { calls++; throw HolonHttpException(503, null, retryAfterSeconds = 2) },
            isCurrent = { true }, onAuthenticationFailure = {},
            dispatcher = StandardTestDispatcher(testScheduler), nowMillis = { testScheduler.currentTime },
        )
        try {
            assertNull(repository.image(key(), true))
            assertEquals(2_000L, testScheduler.currentTime)
            assertEquals(2, calls)
            assertNull(repository.image(key(), true))
            assertEquals(2, calls)
        } finally { repository.close() }
    }

    @Test fun `visible requests recover at cooldown expiry without a new composition key`() = runTest {
        var calls = 0
        val repository = AgentAvatarRepository(
            read = { if (++calls == 1) throw IOException("temporary") else png() },
            isCurrent = { true }, onAuthenticationFailure = {},
            dispatcher = StandardTestDispatcher(testScheduler), nowMillis = { testScheduler.currentTime },
        )
        try {
            assertNotNull(repository.image(key(), true, retry = true))
            assertEquals(2, calls)
            assertEquals(30_000L, testScheduler.currentTime)
        } finally { repository.close() }
    }

    @Test fun `visible retries stop after three rounds`() = runTest {
        var calls = 0
        val repository = AgentAvatarRepository(
            read = { calls++; throw HolonHttpException(404, null) },
            isCurrent = { true }, onAuthenticationFailure = {},
            dispatcher = StandardTestDispatcher(testScheduler), nowMillis = { testScheduler.currentTime },
        )
        try {
            assertNull(repository.image(key(), true, retry = true))
            assertEquals(3, calls)
            assertEquals(60_000L, testScheduler.currentTime)
        } finally { repository.close() }
    }

    @Test fun `going offline prevents a delayed 503 retry even before UI cancellation`() = runTest {
        var calls = 0
        var online = true
        val repository = AgentAvatarRepository(
            read = { calls++; throw HolonHttpException(503, null, retryAfterSeconds = 2) },
            isCurrent = { true }, onAuthenticationFailure = {},
            dispatcher = StandardTestDispatcher(testScheduler), isNetworkAllowed = { online },
        )
        try {
            val request = async { repository.image(key(), true, retry = true) }
            runCurrent()
            assertEquals(1, calls)
            online = false
            advanceTimeBy(2_001)
            assertNull(request.await())
            assertEquals(1, calls)
        } finally { repository.close() }
    }

    @Test fun `offline cancellation preserves cached art and permits later recovery`() = runTest {
        var calls = 0
        val repository = AgentAvatarRepository(
            read = { calls++; if (it.agentId == "cached") png() else throw HolonHttpException(503, null) },
            isCurrent = { true }, onAuthenticationFailure = {},
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        try {
            repository.image(key("cached"), true)
            val request = async { repository.image(key(), true) }
            runCurrent()
            repository.cancelPending()
            assertFailsWith<CancellationException> { request.await() }
            assertNotNull(repository.image(key("cached"), false))
            assertNull(repository.image(key(), false))
            assertEquals(2, calls)
        } finally { repository.close() }
    }

    @Test fun `reconnect does not reuse a cancelled read that has not exited`(): Unit = runBlocking {
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        val calls = AtomicInteger()
        val online = java.util.concurrent.atomic.AtomicBoolean(true)
        val repository = AgentAvatarRepository(
            read = {
                if (calls.incrementAndGet() == 1) {
                    started.countDown()
                    while (release.count > 0) {
                        try {
                            check(release.await(15, TimeUnit.SECONDS))
                        } catch (_: InterruptedException) {
                            // Model a synchronous transport that cannot immediately unwind cancellation.
                        }
                    }
                }
                png()
            },
            isCurrent = { true }, onAuthenticationFailure = {}, isNetworkAllowed = { online.get() },
        )
        try {
            val old = async(start = CoroutineStart.UNDISPATCHED) { repository.image(key(), true) }
            assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
            online.set(false)
            repository.cancelPending()
            online.set(true)
            assertNotNull(withTimeout(5_000) { repository.image(key(), true, retry = true) })
            assertEquals(2, calls.get())
            release.countDown()
            assertFailsWith<CancellationException> { old.await() }
            assertNotNull(repository.image(key(), false))
        } finally {
            release.countDown()
            repository.close()
        }
    }

    @Test fun `long retry after is respected without holding a request open`() = runTest {
        var calls = 0
        val repository = AgentAvatarRepository(
            read = { calls++; throw HolonHttpException(503, null, retryAfterSeconds = 120) },
            isCurrent = { true }, onAuthenticationFailure = {},
            dispatcher = StandardTestDispatcher(testScheduler), nowMillis = { testScheduler.currentTime },
        )
        try {
            assertNull(repository.image(key(), true))
            advanceTimeBy(60_000)
            assertNull(repository.image(key(), true))
            assertEquals(1, calls)
        } finally { repository.close() }
    }

    @Test fun `authentication failure clears art and invalidates only the active lease`() = runTest {
        for (status in listOf(401, 403)) {
            var failed = false
            val repository = AgentAvatarRepository(
                read = { if (it.agentId == "cached") png() else throw HolonHttpException(status, null) },
                isCurrent = { true }, onAuthenticationFailure = { failed = true },
                dispatcher = StandardTestDispatcher(testScheduler),
            )
            assertNotNull(repository.image(key("cached"), true))
            assertFailsWith<CancellationException> { repository.image(key(), true) }
            assertTrue(failed)
            assertFalse(repository.isActive())
            assertNull(repository.image(key("cached"), false))
        }
    }

    @Test fun `late results and authentication errors cannot affect replacement lease`() = runTest {
        for (error in listOf(false, true)) {
            var current = true
            var failed = false
            val repository = AgentAvatarRepository(
                read = { current = false; if (error) throw HolonHttpException(401, null); png() },
                isCurrent = { current }, onAuthenticationFailure = { failed = true },
                dispatcher = StandardTestDispatcher(testScheduler),
            )
            try {
                assertNull(repository.image(key(), true))
                assertFalse(failed)
                assertNull(repository.image(key(), false))
            } finally { repository.close() }
        }
    }

    @Test fun `close cancels pending publication and cannot be reused`() = runTest {
        lateinit var repository: AgentAvatarRepository
        repository = AgentAvatarRepository(
            read = { repository.close(); png() }, isCurrent = { true }, onAuthenticationFailure = {},
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        assertFailsWith<CancellationException> { repository.image(key(), true) }
        assertNull(repository.image(key(), true))
    }

    @Test fun `untrusted image bounds and density tiers are explicit`() {
        assertTrue(validAvatarPng(png(), 64))
        assertFalse(validAvatarPng(png(512), 64))
        assertFalse(validAvatarPng(png(Int.MAX_VALUE), 64))
        assertFalse(validAvatarPng(png().copyOf(24), 64))
        assertFalse(validAvatarPng(png().copyOf(2 * 1024 * 1024 + 1), 64))
        assertFalse(validAvatarPng(ByteArray(33), 64))
        assertEquals(32, avatarPixels(32f, 1f))
        assertEquals(128, avatarPixels(48f, 2f))
        assertEquals(256, avatarPixels(48f, 3f))
        assertEquals(512, avatarPixels(48f, 20f))
    }

    @Test fun `invalid image is not cached or repeatedly decoded`() = runTest {
        var calls = 0
        val repository = AgentAvatarRepository(
            read = { calls++; png(512) }, isCurrent = { true }, onAuthenticationFailure = {},
            dispatcher = StandardTestDispatcher(testScheduler),
        )
        try {
            assertNull(repository.image(key(), true))
            assertNull(repository.image(key(), false))
            assertNull(repository.image(key(), true))
            assertEquals(1, calls)
        } finally { repository.close() }
    }

    @Test fun `queued requests check dynamic connectivity before starting`() = runBlocking {
        val started = CountDownLatch(4)
        val release = CountDownLatch(1)
        val calls = AtomicInteger()
        val online = java.util.concurrent.atomic.AtomicBoolean(true)
        val repository = AgentAvatarRepository(
            read = {
                calls.incrementAndGet()
                started.countDown()
                check(release.await(5, TimeUnit.SECONDS))
                png()
            },
            isCurrent = { true }, onAuthenticationFailure = {}, isNetworkAllowed = { online.get() },
        )
        try {
            val requests = (0..11).map {
                async(start = CoroutineStart.UNDISPATCHED) { repository.image(key("$it"), true) }
            }
            assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
            online.set(false)
            release.countDown()
            assertEquals(4, requests.awaitAll().count { it != null })
            assertEquals(4, calls.get())
        } finally {
            release.countDown()
            repository.close()
        }
    }

    @Test fun `visible request resumes when the bounded queue frees capacity`(): Unit = runBlocking {
        val started = CountDownLatch(4)
        val release = CountDownLatch(1)
        val repository = AgentAvatarRepository(
            read = { started.countDown(); check(release.await(5, TimeUnit.SECONDS)); png() },
            isCurrent = { true }, onAuthenticationFailure = {},
        )
        try {
            val queued = (0 until 32).map {
                async(start = CoroutineStart.UNDISPATCHED) { repository.image(key("$it"), true) }
            }
            assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
            assertNull(repository.image(key("overflow"), true))
            val visible = async(start = CoroutineStart.UNDISPATCHED) {
                repository.image(key("visible"), true, retry = true)
            }
            assertFalse(visible.isCompleted)
            release.countDown()
            queued.awaitAll().forEach { assertNotNull(it) }
            assertNotNull(visible.await())
        } finally {
            release.countDown()
            repository.close()
        }
    }

    @Test fun `at most four network loads run concurrently`() = runBlocking {
        val started = CountDownLatch(4)
        val release = CountDownLatch(1)
        val running = AtomicInteger()
        val maximum = AtomicInteger()
        val repository = AgentAvatarRepository(
            read = {
                val active = running.incrementAndGet()
                maximum.updateAndGet { maxOf(it, active) }
                started.countDown()
                try {
                    check(release.await(5, TimeUnit.SECONDS))
                    png()
                } finally { running.decrementAndGet() }
            },
            isCurrent = { true }, onAuthenticationFailure = {},
        )
        try {
            val requests = (0..11).map { async(Dispatchers.Default) { repository.image(key("$it"), true) } }
            assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
            assertEquals(4, running.get())
            release.countDown()
            requests.awaitAll().forEach { assertNotNull(it) }
            assertEquals(4, maximum.get())
        } finally {
            release.countDown()
            repository.close()
        }
    }
}
