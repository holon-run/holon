package run.holon.android.app

import android.content.Context
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

internal class SessionResetBarrier(private val scope: kotlinx.coroutines.CoroutineScope) {
    private var pending: Job? = null
    private var activeTransitions = 0

    fun beginTransition() {
        synchronized(this) {
            activeTransitions += 1
        }
    }

    fun endTransition() {
        synchronized(this) {
            check(activeTransitions > 0)
            activeTransitions -= 1
        }
    }

    fun schedule(reset: suspend () -> Unit): Job {
        synchronized(this) {
            if (activeTransitions > 0) return scope.launch {}
        }
        val previous = pending
        val next =
            scope.launch {
                previous?.join()
                runCatching { reset() }
            }
        pending = next
        return next
    }

    suspend fun await() {
        while (true) {
            val current = pending ?: return
            current.join()
            if (pending === current) {
                pending = null
                return
            }
        }
    }
}

internal class AppContainer(context: Context) {
    private val database = HolonDatabase.create(context)
    val traceRecorder = TraceRecorder(context)
    val repository =
        HolonRepository(
            context = context,
            sessionStore = createSessionStore(context),
            preferences = HostPreferences(context),
            dao = database.holonDao(),
            traceRecorder = traceRecorder,
        )
}
