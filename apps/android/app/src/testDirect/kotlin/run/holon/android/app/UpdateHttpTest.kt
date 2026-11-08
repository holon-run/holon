package run.holon.android.app

import java.io.IOException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Timeout

class UpdateHttpTest {
    @Test
    fun `cancelling a metadata or download request cancels the underlying call`() = runBlocking {
        val call = PendingCall()
        val job = launch(start = CoroutineStart.UNDISPATCHED) {
            call.readCancellable { _, _ -> error("No response expected") }
        }
        job.cancelAndJoin()
        assertTrue(call.isCanceled())
    }

    @Test
    fun `rejects bounded metadata even when length is unknown`() {
        val content = object : okhttp3.ResponseBody() {
            override fun contentType() = null
            override fun contentLength() = -1L
            override fun source() = okio.Buffer().writeUtf8("too large")
        }
        val response = response(content)
        response.use {
            assertFailsWith<IOException> { it.readBoundedText(4) {} }
        }
    }

    @Test
    fun `reads bounded metadata and probes cancellation`() {
        var checks = 0
        response("{}".toResponseBody()).use {
            assertEquals("{}", it.readBoundedText(4) { checks++ })
        }
        assertTrue(checks > 0)
    }

    private fun response(body: okhttp3.ResponseBody) = Response.Builder()
        .request(Request.Builder().url("https://example.test/metadata.json").build())
        .protocol(Protocol.HTTP_1_1)
        .code(200)
        .message("OK")
        .body(body)
        .build()

    private class PendingCall : Call {
        private var cancelled = false
        override fun request() = Request.Builder().url("https://example.test/update.apk").build()
        override fun execute(): Response = error("Use enqueue")
        override fun enqueue(responseCallback: Callback) = Unit
        override fun cancel() { cancelled = true }
        override fun isExecuted() = true
        override fun isCanceled() = cancelled
        override fun timeout() = Timeout()
        override fun clone(): Call = PendingCall()
    }
}
