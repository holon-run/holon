package run.holon.android.app

import java.io.IOException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runInterruptible
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import run.holon.android.sdk.HolonAvatarExpression
import run.holon.android.sdk.HolonHttpException

internal data class AgentAvatarKey(val agentId: String, val expression: HolonAvatarExpression, val pixels: Int)

/** One authenticated lease only. Neither cached bytes nor late responses cross its boundary. */
internal class AgentAvatarRepository(
    private val read: (AgentAvatarKey) -> ByteArray,
    private val isCurrent: () -> Boolean,
    private val onAuthenticationFailure: (Throwable) -> Unit,
    dispatcher: CoroutineDispatcher = Dispatchers.IO,
    private val nowMillis: () -> Long = { System.nanoTime() / 1_000_000 },
    private val maxEntries: Int = 128,
    private val maxBytes: Int = 8 * 1024 * 1024,
    private val isNetworkAllowed: () -> Boolean = { true },
) : AutoCloseable {
    private val lock = Any()
    private val scope = CoroutineScope(SupervisorJob() + dispatcher)
    private val slots = Semaphore(4)
    private val images = LinkedHashMap<AgentAvatarKey, ByteArray>(16, .75f, true)
    private val inFlight = mutableMapOf<AgentAvatarKey, Deferred<ByteArray?>>()
    private val retryAt = LinkedHashMap<AgentAvatarKey, Long>()
    private val completions = MutableStateFlow(0L)
    private var bytes = 0
    @Volatile private var closed = false

    fun isActive(): Boolean = !closed && isCurrent()

    private fun canFetch(): Boolean = isActive() && isNetworkAllowed()

    suspend fun image(key: AgentAvatarKey, allowNetwork: Boolean, retry: Boolean = false): ByteArray? {
        // Only visible consumers wait for recovery, and never retry indefinitely.
        repeat(if (retry) 3 else 1) { attempt ->
            val image = load(key, allowNetwork)
            if (image != null || !retry || attempt == 2 || !allowNetwork || !canFetch()) return image
            val cooldown = synchronized(lock) { retryAt[key]?.minus(nowMillis()) }
            if (cooldown != null && cooldown > 0) {
                delay(cooldown)
            } else {
                completions.first { !canFetch() || synchronized(lock) { inFlight.size < 32 } }
            }
        }
        return null
    }

    private suspend fun load(key: AgentAvatarKey, allowNetwork: Boolean): ByteArray? {
        if (!isActive()) return null
        val pending = synchronized(lock) {
            if (!isActive()) return null
            images[key]?.let { return it }
            if (!allowNetwork || !canFetch() || (retryAt[key] ?: Long.MIN_VALUE) > nowMillis()) return null
            inFlight[key] ?: run {
                // Visible images only; a very large roster cannot build an unbounded queue.
                if (inFlight.size >= 32) return null
                scope.async(start = kotlinx.coroutines.CoroutineStart.LAZY) { fetch(key) }
                    .also { request ->
                        inFlight[key] = request
                        request.invokeOnCompletion {
                            synchronized(lock) {
                                if (inFlight[key] === request) inFlight.remove(key)
                                completions.value++
                            }
                        }
                    }
            }
        }
        return pending.await().takeIf { isActive() }
    }

    fun reject(key: AgentAvatarKey) = synchronized(lock) {
        images.remove(key)?.let { bytes -= it.size }
        deferRetry(key, 30_000)
    }

    fun cancelPending() {
        val pending = synchronized(lock) {
            // A cancelled blocking read may take time to exit; never reuse it on reconnect.
            inFlight.values.toList().also {
                inFlight.clear()
                completions.value++
            }
        }
        pending.forEach { it.cancel() }
    }

    private suspend fun fetch(key: AgentAvatarKey): ByteArray? {
        return slots.withPermit {
            var retried = false
            while (true) {
                if (!canFetch()) return@withPermit null
                try {
                    val image = runInterruptible {
                        if (!canFetch()) throw CancellationException("Avatar network access paused")
                        read(key)
                    }
                    currentCoroutineContext().ensureActive()
                    if (!validAvatarPng(image, key.pixels)) throw IOException("Invalid avatar PNG")
                    return@withPermit synchronized(lock) {
                        if (!isActive()) return@synchronized null
                        if (image.size <= maxBytes) {
                            images.put(key, image)?.let { bytes -= it.size }
                            bytes += image.size
                            retryAt.remove(key)
                            val oldest = images.entries.iterator()
                            while (images.size > maxEntries || bytes > maxBytes) {
                                bytes -= oldest.next().value.size
                                oldest.remove()
                            }
                        }
                        image
                    }
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Exception) {
                    if (!canFetch()) return@withPermit null
                    val http = error as? HolonHttpException
                    if (http?.statusCode in setOf(401, 403)) {
                        close()
                        onAuthenticationFailure(error)
                        return@withPermit null
                    }
                    val retrySeconds = http?.retryAfterSeconds?.coerceAtLeast(1) ?: 1
                    if (http?.statusCode == 503 && !retried && retrySeconds <= 5) {
                        retried = true
                        delay(retrySeconds * 1_000)
                    } else {
                        synchronized(lock) {
                            val cooldown = if (http?.statusCode == 503) {
                                maxOf(30_000, retrySeconds.coerceAtMost(86_400) * 1_000)
                            } else 30_000
                            deferRetry(key, cooldown)
                        }
                        return@withPermit null
                    }
                }
            }
            @Suppress("UNREACHABLE_CODE")
            null
        }
    }

    private fun deferRetry(key: AgentAvatarKey, millis: Long) {
        retryAt[key] = nowMillis() + millis
        while (retryAt.size > maxEntries) retryAt.remove(retryAt.keys.first())
    }

    override fun close() {
        synchronized(lock) {
            closed = true
            images.clear()
            inFlight.clear()
            retryAt.clear()
            bytes = 0
        }
        scope.cancel()
    }
}

/** Check dimensions before either caching or passing untrusted image bytes to a decoder. */
internal fun validAvatarPng(bytes: ByteArray, pixels: Int): Boolean {
    if (pixels !in listOf(32, 64, 128, 256, 512) || bytes.size !in 33..(2 * 1024 * 1024)) return false
    val signature = byteArrayOf(0x89.toByte(), 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)
    if (!bytes.take(8).toByteArray().contentEquals(signature)) return false
    fun intAt(offset: Int): Int = (0..3).fold(0) { result, index ->
        (result shl 8) or (bytes[offset + index].toInt() and 0xff)
    }
    return intAt(8) == 13 && intAt(12) == 0x49484452 &&
        intAt(16) == pixels && intAt(20) == pixels
}

internal fun avatarPixels(dp: Float, density: Float): Int =
    listOf(32, 64, 128, 256, 512).firstOrNull { it >= dp * density } ?: 512
