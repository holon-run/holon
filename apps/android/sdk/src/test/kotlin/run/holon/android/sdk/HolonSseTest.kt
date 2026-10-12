package run.holon.android.sdk

import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotEquals
import kotlin.test.assertSame
import kotlin.test.assertTrue
import okhttp3.MediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.ResponseBody
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okio.Buffer
import okio.BufferedSink
import okio.BufferedSource
import okio.Pipe
import okio.buffer

class HolonSseTest {
    @Test
    fun `closing a new connection cancels before background body cleanup`() {
        assertUnreadClose(claimBeforeClose = false)
    }

    @Test
    fun `closing a claimed connection cancels before background body cleanup`() {
        assertUnreadClose(claimBeforeClose = true)
    }

    @Test
    fun `background cleanup contains IO failures and continues on the same worker`() {
        assertBackgroundCloseFailure(IOException("body close failed"))
    }

    @Test
    fun `background cleanup contains runtime failures and continues on the same worker`() {
        assertBackgroundCloseFailure(IllegalStateException("body close failed"))
    }

    @Test
    fun `closing an unread connection does not wait for body cleanup`() {
        val cleanupStarted = CountDownLatch(1)
        val allowCleanup = CountDownLatch(1)
        val cancellations = AtomicInteger()
        val body =
            RecordingBody(Buffer()) {
                assertEquals(1, cancellations.get())
                cleanupStarted.countDown()
                assertTrue(allowCleanup.await(2, TimeUnit.SECONDS))
            }
        val connection = HolonSseConnection(body, deduplicator = null) {
            cancellations.incrementAndGet()
        }

        try {
            connection.close()
            assertTrue(cleanupStarted.await(2, TimeUnit.SECONDS))
            assertEquals(1, cancellations.get())
            assertEquals(1L, body.closed.count)
        } finally {
            allowCleanup.countDown()
        }
        body.assertClosed()
    }

    @Test
    fun `concurrent and repeated close cancels and cleans up only once`() {
        val cancellations = AtomicInteger()
        val body = RecordingBody(Buffer()) { assertEquals(1, cancellations.get()) }
        val connection = HolonSseConnection(body, deduplicator = null) {
            cancellations.incrementAndGet()
        }
        val start = CountDownLatch(1)
        val closers =
            List(8) {
                thread {
                    start.await()
                    repeat(3) { connection.close() }
                }
            }

        start.countDown()
        closers.forEach {
            it.join(2_000)
            assertTrue(!it.isAlive)
        }
        body.assertClosed()
        connection.close()
        assertEquals(1, cancellations.get())
        assertEquals(1, body.closeCalls.get())
    }

    @Test
    fun `closing a consumed connection cancels the call and lets the reader own the body`() {
        val pipe = Pipe(8_192L)
        val sink: BufferedSink = pipe.sink.buffer()
        val cancellations = AtomicInteger()
        val firstEvent = CountDownLatch(1)
        val body = RecordingBody(pipe.source.buffer()) { assertEquals(1, cancellations.get()) }
        sink.writeUtf8("id: event-1\ndata: hello\n\n").flush()
        val connection = HolonSseConnection(body, deduplicator = null) {
            cancellations.incrementAndGet()
            pipe.cancel()
        }

        val reader =
            thread(name = "sse-reader") {
                runCatching {
                    connection.events().onEach { firstEvent.countDown() }.toList()
                }
            }

        assertTrue(firstEvent.await(2, TimeUnit.SECONDS))
        connection.close()
        connection.close()
        reader.join(2_000)

        assertTrue(!reader.isAlive)
        body.assertClosed()
        assertEquals(1, cancellations.get())
        assertEquals(reader, body.closedOn.get())
    }

    @Test
    fun `closing while reading starts gives exactly one owner of body cleanup`() {
        repeat(25) {
            val pipe = Pipe(8_192L)
            val cancellations = AtomicInteger()
            val body = RecordingBody(pipe.source.buffer()) { assertEquals(1, cancellations.get()) }
            val connection = HolonSseConnection(body, deduplicator = null) {
                cancellations.incrementAndGet()
                pipe.cancel()
            }
            val events = connection.events()
            val start = CountDownLatch(1)
            val readerFailure = AtomicReference<Throwable?>()
            val reader =
                thread(name = "sse-racing-reader") {
                    start.await()
                    readerFailure.set(runCatching { events.toList() }.exceptionOrNull())
                }
            val closer =
                thread {
                    start.await()
                    connection.close()
                }

            start.countDown()
            reader.join(2_000)
            closer.join(2_000)

            assertTrue(!reader.isAlive)
            assertTrue(!closer.isAlive)
            body.assertClosed()
            assertEquals(1, cancellations.get())
            val failure = readerFailure.get()
            assertTrue(failure is IOException || failure is IllegalStateException)
            if (failure is IllegalStateException) {
                assertEquals("SSE connection was closed before it could be read", failure.message)
                assertNotEquals(reader, body.closedOn.get())
            } else {
                assertEquals(reader, body.closedOn.get())
            }
        }
    }

    @Test
    fun `closing an unread chunked response cancels before off-thread cleanup`() {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setHeader("Content-Type", "text/event-stream")
                    .setChunkedBody("data: hello\n\n", 1)
                    .setBodyDelay(250, TimeUnit.MILLISECONDS),
            )
            val client = OkHttpClient()
            try {
                val call = client.newCall(Request.Builder().url(server.url("/events")).build())
                val response = call.execute()
                val body = RecordingBody(response.body!!.source()) { assertTrue(call.isCanceled()) }
                val connection = HolonSseConnection(body, deduplicator = null, cancelCall = call::cancel)
                val caller = Thread.currentThread()

                connection.close()

                assertTrue(call.isCanceled())
                body.assertClosed()
                assertNotEquals(caller, body.closedOn.get())
            } finally {
                client.dispatcher.executorService.shutdown()
                client.connectionPool.evictAll()
            }
        }
    }

    private fun assertUnreadClose(claimBeforeClose: Boolean) {
        val caller = Thread.currentThread()
        val cancellations = AtomicInteger()
        val body =
            RecordingBody(Buffer().writeUtf8("data: hello\n\n")) {
                assertEquals(1, cancellations.get())
            }
        val connection = HolonSseConnection(body, deduplicator = null) {
            cancellations.incrementAndGet()
        }
        val events = if (claimBeforeClose) connection.events() else null

        connection.close()

        assertEquals(1, cancellations.get())
        body.assertClosed()
        assertNotEquals(caller, body.closedOn.get())
        assertFailsWith<IllegalStateException> { connection.events() }
        if (events != null) {
            assertFailsWith<IllegalStateException> { events.toList() }
        }
        connection.close()
        assertEquals(1, cancellations.get())
        assertEquals(1, body.closeCalls.get())
    }

    private fun assertBackgroundCloseFailure(failure: Exception) {
        for (claimBeforeClose in listOf(false, true)) {
            val cancellations = AtomicInteger()
            val body =
                RecordingBody(Buffer()) {
                    assertEquals(1, cancellations.get())
                    throw failure
                }
            val connection = HolonSseConnection(body, deduplicator = null) {
                cancellations.incrementAndGet()
            }
            if (claimBeforeClose) {
                connection.events()
            }

            connection.close()
            val nextBody = RecordingBody(Buffer())
            HolonSseConnection(nextBody, deduplicator = null, cancelCall = {}).close()
            nextBody.assertClosed()
            body.assertClosed(expectedFailure = failure)
            // An escaping task failure replaces the executor's worker thread.
            assertSame(body.closedOn.get(), nextBody.closedOn.get())
            assertNotEquals(Thread.currentThread(), body.closedOn.get())
            connection.close()
            assertEquals(1, cancellations.get())
            assertEquals(1, body.closeCalls.get())
        }
    }

    private class RecordingBody(
        private val dataSource: BufferedSource,
        private val beforeClose: () -> Unit = {},
    ) : ResponseBody() {
        val closed = CountDownLatch(1)
        val closedOn = AtomicReference<Thread?>()
        val closeCalls = AtomicInteger()
        private val closeFailure = AtomicReference<Throwable?>()

        override fun contentType(): MediaType? = null

        override fun contentLength(): Long = -1L

        override fun source(): BufferedSource = dataSource

        override fun close() {
            closeCalls.incrementAndGet()
            closedOn.set(Thread.currentThread())
            try {
                beforeClose()
                super.close()
            } catch (failure: Throwable) {
                closeFailure.set(failure)
                throw failure
            } finally {
                closed.countDown()
            }
        }

        fun assertClosed(expectedFailure: Throwable? = null) {
            assertTrue(closed.await(2, TimeUnit.SECONDS))
            assertSame(expectedFailure, closeFailure.get())
            assertEquals(1, closeCalls.get())
        }
    }
}
