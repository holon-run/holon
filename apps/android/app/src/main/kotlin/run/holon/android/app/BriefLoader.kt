package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit

internal sealed interface BriefLoadState {
    data object Loading : BriefLoadState
    data class Failed(val message: String) : BriefLoadState
}

/** Owned by the UI dispatcher. Changing identity cancels both active and queued reads. */
internal class BriefLoader<T>(
    private val scope: CoroutineScope,
    concurrency: Int = 3,
    private val read: suspend (String) -> T,
    private val onLoading: (String) -> Unit,
    private val onLoaded: (String, T) -> Unit,
    private val onFailure: (String, Throwable) -> Unit,
) {
    private val permits = Semaphore(concurrency)
    private val jobs = mutableMapOf<String, Job>()
    private val requested = mutableSetOf<String>()
    private var generation = 0L

    fun reset() {
        generation++
        jobs.values.forEach(Job::cancel)
        jobs.clear()
        requested.clear()
    }

    fun request(ids: List<String>, retry: Boolean = false) {
        val expected = generation
        ids.distinct().forEach { id ->
            if (id in jobs || (!retry && id in requested)) return@forEach
            requested.add(id)
            onLoading(id)
            val job = scope.launch(start = CoroutineStart.LAZY) {
                try {
                    val value = permits.withPermit { read(id) }
                    if (expected == generation) onLoaded(id, value)
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Throwable) {
                    if (expected == generation) onFailure(id, error)
                } finally {
                    if (expected == generation) jobs.remove(id)
                }
            }
            jobs[id] = job
            job.start()
        }
    }
}
