package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import run.holon.android.sdk.HolonBriefReadState

/** UI-dispatcher owned. Callers share active work without owning its cancellation. */
internal class RosterRefreshRequest<T>(
    private val scope: CoroutineScope,
    private val read: suspend () -> T,
) {
    private val lock = Any()
    private var request: Deferred<T>? = null

    fun load(): Deferred<T> =
        synchronized(lock) {
            request?.takeIf { it.isActive }?.let { return@synchronized it }
            scope.async(start = CoroutineStart.LAZY) { read() }.also {
                request = it
                it.start()
            }
        }

    fun reset() {
        synchronized(lock) {
            request?.cancel()
            request = null
        }
    }
}

internal sealed interface BriefReadSnapshot {
    data class Server(val states: Map<String, HolonBriefReadState>) : BriefReadSnapshot
    data class Legacy(val ids: Map<String, String>) : BriefReadSnapshot
}

/** One read plus one trailing refresh; transient failures retry until stopped or recovered. */
internal class BriefReadStateLoader(
    private val scope: CoroutineScope,
    private val read: suspend () -> BriefReadSnapshot,
    private val onLoaded: (BriefReadSnapshot) -> Unit,
    private val onFailure: (Throwable) -> Unit,
) {
    private val lock = Any()
    private var job: Job? = null
    private var generation = 0L
    private var pending = false

    fun reset() {
        synchronized(lock) {
            generation++
            job?.cancel()
            job = null
            pending = false
        }
    }

    fun request() {
        synchronized(lock) {
            if (job?.isActive == true) {
                pending = true
                return
            }
            val expected = generation
            job =
                scope.launch(start = CoroutineStart.LAZY) {
                    runRequest(expected)
                }.also { it.start() }
        }
    }

    private suspend fun runRequest(expected: Long) {
        var retryDelay = 1_000L
        var reportedFailure = false
        while (true) {
            synchronized(lock) {
                if (expected != generation) return
                pending = false
            }
            try {
                val value = read()
                synchronized(lock) {
                    if (expected != generation) return
                }
                onLoaded(value)
                reportedFailure = false
                retryDelay = 1_000L
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Throwable) {
                val (shouldReport, shouldRetry) =
                    synchronized(lock) {
                        if (expected != generation) return
                        val transient = error.isTransientNetworkFailure()
                        (!reportedFailure || !transient) to transient
                    }
                if (shouldReport) onFailure(error)
                reportedFailure = true
                if (!shouldRetry) return
                delay(retryDelay)
                synchronized(lock) {
                    if (expected != generation) return
                    retryDelay = (retryDelay * 2).coerceAtMost(30_000L)
                    pending = true
                }
            }

            synchronized(lock) {
                if (expected != generation) return
                if (!pending) {
                    job = null
                    return
                }
            }
        }
    }
}
