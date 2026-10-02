package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
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

/** Event-driven hydration: at most four reads, with one trailing read per Agent. */
internal class AgentPreviewLoader<T>(
    private val scope: CoroutineScope,
    private val read: suspend (String) -> T,
    private val onLoaded: (String, T) -> Unit,
    private val onFailure: (Throwable) -> Unit,
) {
    private val permits = Semaphore(4)
    private val jobs = mutableMapOf<String, Job>()
    private val pending = mutableSetOf<String>()
    private var generation = 0L

    fun request(agentId: String) {
        pending.add(agentId)
        if (jobs[agentId]?.isActive == true) return
        val expected = generation
        jobs[agentId] = scope.launch {
            while (expected == generation && pending.remove(agentId)) {
                delay(250)
                try {
                    val value = permits.withPermit { read(agentId) }
                    if (expected != generation) return@launch
                    onLoaded(agentId, value)
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Throwable) {
                    if (expected != generation) return@launch
                    onFailure(error)
                }
            }
            if (expected == generation) jobs.remove(agentId)
        }
    }

    fun reset() {
        generation++
        jobs.values.forEach(Job::cancel)
        jobs.clear()
        pending.clear()
    }
}

/** Foreground-only, monotonic/coalesced read receipts; failures never create UI banners. */
internal class BriefReadStateWriter(
    private val scope: CoroutineScope,
    private val write: suspend (String, Long) -> Unit,
    private val onFailure: (Throwable) -> Unit,
) {
    private val jobs = mutableMapOf<String, Job>()
    private val targets = mutableMapOf<String, Long>()
    private val acknowledged = mutableMapOf<String, Long>()
    private val deferred = mutableMapOf<String, Long>()
    private var generation = 0L

    fun request(agentId: String, through: Long) {
        if (through <= (acknowledged[agentId] ?: -1L)) return
        if (through <= (deferred[agentId] ?: -1L)) return
        targets[agentId] = maxOf(through, targets[agentId] ?: -1L)
        if (jobs[agentId]?.isActive == true) return
        val expected = generation
        jobs[agentId] = scope.launch {
            var retryDelay = 1_000L
            while (true) {
                if (expected != generation) return@launch
                val target = targets[agentId] ?: break
                try {
                    write(agentId, target)
                    if (expected != generation) return@launch
                    acknowledged[agentId] = maxOf(target, acknowledged[agentId] ?: -1L)
                    if (targets[agentId] == target) targets.remove(agentId)
                    retryDelay = 1_000L
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Throwable) {
                    if (expected != generation) return@launch
                    onFailure(error)
                    if (expected != generation) return@launch
                    if (!error.isTransientNetworkFailure()) {
                        deferred[agentId] = target
                        targets.remove(agentId)
                        break
                    }
                    delay(retryDelay)
                    retryDelay = (retryDelay * 2).coerceAtMost(30_000L)
                }
            }
            jobs.remove(agentId)
        }
    }

    fun reset() {
        generation++
        jobs.values.forEach(Job::cancel)
        jobs.clear()
        targets.clear()
        acknowledged.clear()
        deferred.clear()
    }
}
