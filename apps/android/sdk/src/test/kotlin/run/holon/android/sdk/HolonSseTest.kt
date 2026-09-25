package run.holon.android.sdk

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import okhttp3.MediaType
import okhttp3.ResponseBody
import okio.BufferedSink
import okio.BufferedSource
import okio.Pipe
import okio.buffer

class HolonSseTest {
    @Test
    fun `closing a claimed connection closes an unread body immediately`() {
        val source = okio.Buffer().writeUtf8("data: hello\n\n")
        val bodyClosedOn = AtomicReference<String?>()
        val body = recordingBody(source, bodyClosedOn)
        var canceled = false
        val connection = HolonSseConnection(body, deduplicator = null) { canceled = true }
        val events = connection.events()

        connection.close()

        assertTrue(canceled)
        assertEquals(Thread.currentThread().name, bodyClosedOn.get())
        assertFailsWith<IllegalStateException> { events.toList() }
    }

    @Test
    fun `closing a consumed connection cancels the call and lets the reader own the body`() {
        val pipe = Pipe(8_192L)
        val sink: BufferedSink = pipe.sink.buffer()
        val source: BufferedSource = pipe.source.buffer()
        val bodyClosedOn = AtomicReference<String?>()
        val firstEvent = CountDownLatch(1)
        val body = recordingBody(source, bodyClosedOn)
        sink.writeUtf8("id: event-1\ndata: hello\n\n").flush()
        val connection = HolonSseConnection(body, deduplicator = null, cancelCall = pipe::cancel)

        val reader =
            thread(name = "sse-reader") {
                runCatching {
                    connection.events().onEach { firstEvent.countDown() }.toList()
                }
            }

        assertTrue(firstEvent.await(2, TimeUnit.SECONDS))
        connection.close()
        reader.join(2_000)

        assertTrue(!reader.isAlive)
        assertEquals("sse-reader", bodyClosedOn.get())
    }

    private fun recordingBody(
        source: BufferedSource,
        closedOn: AtomicReference<String?>,
    ): ResponseBody =
        object : ResponseBody() {
            override fun contentType(): MediaType? = null

            override fun contentLength(): Long = -1L

            override fun source(): BufferedSource = source

            override fun close() {
                closedOn.set(Thread.currentThread().name)
                super.close()
            }
        }
}
