package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.suspendCancellableCoroutine
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.Response
import java.io.ByteArrayOutputStream
import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

internal fun isSecureUpdateUrl(url: String): Boolean =
    url.toHttpUrlOrNull()?.let { it.isHttps && it.username.isEmpty() && it.password.isEmpty() } == true

// Keep cancellation attached until the response body has been consumed, not just until headers arrive.
internal suspend fun <T> Call.readCancellable(read: (Response, () -> Unit) -> T): T =
    suspendCancellableCoroutine { continuation ->
        continuation.invokeOnCancellation { cancel() }
        enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) {
                if (continuation.isActive) continuation.resumeWithException(e)
            }

            override fun onResponse(call: Call, response: Response) {
                try {
                    val result = response.use {
                        if (!it.request.url.isHttps) throw IOException("Update downloads require HTTPS.")
                        read(it) {
                            if (!continuation.isActive) throw CancellationException("Update cancelled")
                        }
                    }
                    if (continuation.isActive) continuation.resume(result)
                } catch (error: Exception) {
                    if (continuation.isActive) continuation.resumeWithException(error)
                }
            }
        })
    }

internal fun Response.readBoundedText(maxBytes: Int, checkActive: () -> Unit): String {
    val content = body ?: throw IOException("The update server returned an empty response.")
    if (content.contentLength() > maxBytes) throw IOException("Update metadata is too large.")
    return content.byteStream().use { input ->
        val output = ByteArrayOutputStream()
        val buffer = ByteArray(8192)
        while (true) {
            checkActive()
            val count = input.read(buffer)
            if (count < 0) break
            if (output.size() + count > maxBytes) throw IOException("Update metadata is too large.")
            output.write(buffer, 0, count)
        }
        output.toString(Charsets.UTF_8.name())
    }
}
