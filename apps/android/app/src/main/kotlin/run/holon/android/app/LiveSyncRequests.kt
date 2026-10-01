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
    private var request: Deferred<T>? = null

    fun load(): Deferred<T> {
        request?.takeIf { it.isActive }?.let { return it }
        return scope.async(start = CoroutineStart.LAZY) { read() }.also {
            request = it
            it.start()
        }
    }

    fun reset() {
        request?.cancel()
        request = null
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
    private var job: Job? = null
    private var generation = 0L
    private var pending = false

    fun reset() {
        generation++
        job?.cancel()
        job = null
        pending = false
    }

    fun request() {
        if (job?.isActive == true) {
            pending = true
            return
        }
        val expected = generation
        job = scope.launch(start = CoroutineStart.LAZY) {
            var retryDelay = 1_000L
            var reportedFailure = false
            do {
                pending = false
                try {
                    val value = read()
                    if (expected != generation) return@launch
                    onLoaded(value)
                    reportedFailure = false
                    retryDelay = 1_000L
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Throwable) {
                    if (expected != generation) return@launch
                    if (!reportedFailure) onFailure(error)
                    reportedFailure = true
                    if (expected != generation || !error.isTransientNetworkFailure()) return@launch
                    delay(retryDelay)
                    if (expected != generation) return@launch
                    retryDelay = (retryDelay * 2).coerceAtMost(30_000L)
                    pending = true
                }
            } while (pending && expected == generation)
        }.also { it.start() }
    }
}
