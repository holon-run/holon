package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/** UI-owned single flight. Only foreground callers request work; reset fences late IO. */
internal class RosterRefreshCoordinator<T>(
    private val scope: CoroutineScope,
    private val read: suspend () -> T,
    private val onLoaded: (T) -> Unit,
    private val onFailure: (Throwable) -> Unit,
    private val onRefreshing: (Boolean) -> Unit,
) {
    private var job: Job? = null
    private var generation = 0L
    private var reading = false
    private val retryNow = Channel<Unit>(Channel.CONFLATED)

    fun request(showProgress: Boolean = false) {
        if (job?.isActive == true) {
            if (showProgress) {
                if (reading) onRefreshing(true) else retryNow.trySend(Unit)
            }
            return
        }
        val expected = generation
        job = scope.launch(start = CoroutineStart.LAZY) {
            var retryDelay = 1_000L
            var progress = showProgress
            try {
                while (expected == generation) {
                    reading = true
                    onRefreshing(progress)
                    try {
                        val value = read()
                        if (expected != generation) return@launch
                        onLoaded(value)
                        return@launch
                    } catch (cancelled: CancellationException) {
                        throw cancelled
                    } catch (error: Throwable) {
                        if (expected != generation) return@launch
                        onFailure(error)
                        if (!error.isTransientNetworkFailure()) return@launch
                    } finally {
                        if (expected == generation) {
                            reading = false
                            onRefreshing(false)
                        }
                    }
                    // Hints do not reset backoff. An explicit retry can wake it immediately.
                    progress = withTimeoutOrNull(retryDelay) { retryNow.receive(); true } ?: false
                    retryDelay = (retryDelay * 2).coerceAtMost(30_000L)
                }
            } finally {
                if (expected == generation) {
                    job = null
                    reading = false
                    onRefreshing(false)
                }
            }
        }.also { it.start() }
    }

    fun reset() {
        generation++
        job?.cancel()
        job = null
        reading = false
        while (retryNow.tryReceive().isSuccess) Unit
        onRefreshing(false)
    }
}
