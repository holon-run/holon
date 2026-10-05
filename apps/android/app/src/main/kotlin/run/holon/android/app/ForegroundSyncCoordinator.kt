package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal interface RosterHintConnection : AutoCloseable {
    fun hints(): Sequence<String>
}

/** One roster hint stream, irrespective of roster size. Detail uses one selected conversation stream. */
internal class ForegroundSyncCoordinator(
    private val scope: CoroutineScope,
    private val open: () -> RosterHintConnection,
    private val onConnected: () -> Unit,
    private val onHint: (String) -> Unit,
    private val onFailure: (Throwable) -> Unit,
    private val reader: CoroutineDispatcher = Dispatchers.IO,
    private val callbacks: CoroutineDispatcher = Dispatchers.Main,
) {
    private var job: Job? = null
    @Volatile private var connection: RosterHintConnection? = null
    @Volatile private var generation = 0L

    fun start(expectedGeneration: Long) {
        if (generation == expectedGeneration && job?.isActive == true) return
        stop()
        generation = expectedGeneration
        job = scope.launch(reader) {
            var retryDelay = 1_000L
            while (isActive && generation == expectedGeneration) {
                try {
                    val opened = open()
                    connection = opened
                    try {
                        if (!isActive || generation != expectedGeneration) break
                        withContext(callbacks) { if (generation == expectedGeneration) onConnected() }
                        for (id in opened.hints()) {
                            if (!isActive || generation != expectedGeneration) break
                            withContext(callbacks) { if (generation == expectedGeneration) onHint(id) }
                            retryDelay = 1_000L
                        }
                    } finally {
                        opened.close()
                        if (connection === opened) connection = null
                    }
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Throwable) {
                    if (!isActive || generation != expectedGeneration) break
                    withContext(callbacks) { if (generation == expectedGeneration) onFailure(error) }
                    if (error.isAuthenticationFailure() || !error.isTransientNetworkFailure()) break
                }
                delay(retryDelay)
                retryDelay = (retryDelay * 2).coerceAtMost(30_000)
            }
        }
    }

    fun stop() {
        generation++
        job?.cancel()
        job = null
        // Cancelling a coroutine alone cannot interrupt a blocking SSE read.
        connection?.close()
        connection = null
    }
}
