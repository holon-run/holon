package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import run.holon.android.sdk.ConversationHistoryBoundaryRequiredException
import run.holon.android.sdk.HolonHttpException

internal fun HolonHttpException.isConversationHistoryResetRequired(): Boolean {
    val error = apiError
    return statusCode == 409 && (error == null || error.code == "conversation_reset_required")
}

/** UI-owned recovery; callers move blocking reads off the UI dispatcher. One instance per selection. */
internal class ConversationSyncCoordinator<T>(
    private val scope: CoroutineScope,
    private val isCurrent: () -> Boolean,
    private val stream: suspend (String?, suspend (T) -> Unit) -> Unit,
    private val snapshot: suspend (Boolean) -> T,
    private val cursorOf: (T) -> String?,
    private val onSnapshot: (T, Boolean, Boolean) -> Unit,
    private val onState: (ConversationSyncState) -> Unit,
    private val onFailure: (Throwable) -> Unit,
    private val closeStream: () -> Unit,
    private val trace: (String, Throwable?, Boolean, Int) -> Unit = { _, _, _, _ -> },
) {
    private var streamJob: Job? = null
    private var fallbackJob: Job? = null
    private val snapshotMutex = Mutex()
    private var stopped = false
    private var cursor: String? = null
    private var revision = 0L
    private var resetPending = false
    private var syncState = ConversationSyncState.Reconnecting

    val isActive: Boolean get() = streamJob?.isActive == true || fallbackJob?.isActive == true

    fun start(after: String?) {
        check(streamJob == null && !stopped)
        cursor = after
        changeState(ConversationSyncState.Reconnecting)
        startFallback()
        streamJob = scope.launch {
            var retryDelay = 1_000L
            var resetAttempted = false
            var resetCount = 0
            while (currentCoroutineContext().isActive && current()) {
                var immediateReset = false
                try {
                    // Failed refreshes never permit reopening with the rejected cursor.
                    if (resetPending && !refreshSnapshot()) {
                        delay(retryDelay)
                        retryDelay = (retryDelay * 2).coerceAtMost(30_000L)
                        continue
                    }
                    if (!current()) break
                    stream(cursor) { bundle ->
                        if (current()) {
                            revision++
                            cursor = cursorOf(bundle)
                            resetPending = false
                            resetAttempted = false
                            retryDelay = 1_000L
                            onSnapshot(bundle, false, true)
                            changeState(ConversationSyncState.Live)
                            fallbackJob?.cancel()
                            fallbackJob = null
                        }
                    }
                    if (!current()) break
                    changeState(ConversationSyncState.Reconnecting)
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Throwable) {
                    if (!current()) break
                    trace("conversation.stream.open_failed", error, cursor != null, resetCount)
                    if (error.isAuthenticationFailure() || error is SessionScopeChangedException) {
                        stop()
                        onFailure(error)
                        break
                    }
                    if (error is ConversationHistoryBoundaryRequiredException ||
                        error is HolonHttpException && error.isConversationHistoryResetRequired()
                    ) {
                        val hadCursor = cursor != null
                        cursor = null
                        resetPending = true
                        revision++
                        resetCount++
                        immediateReset = !resetAttempted
                        resetAttempted = true
                        changeState(ConversationSyncState.NeedsRefresh)
                        trace("conversation.cursor.reset", error, hadCursor, resetCount)
                    } else if (!error.isTransientNetworkFailure()) {
                        // Unknown business conflicts do not get unbounded stream retries.
                        changeState(ConversationSyncState.NeedsRefresh)
                        startFallback()
                        break
                    } else {
                        changeState(ConversationSyncState.Reconnecting)
                    }
                }
                startFallback()
                if (!immediateReset) {
                    delay(retryDelay)
                    retryDelay = (retryDelay * 2).coerceAtMost(30_000L)
                }
            }
        }
    }

    private fun current(): Boolean = !stopped && isCurrent()

    private fun changeState(next: ConversationSyncState) {
        if (!current()) return
        syncState = next
        onState(next)
        trace("conversation.sync.state_changed", null, cursor != null, 0)
    }

    private fun startFallback() {
        if (fallbackJob?.isActive == true || !current()) return
        fallbackJob = scope.launch {
            var backoff = 2_000L
            while (currentCoroutineContext().isActive && current() && syncState != ConversationSyncState.Live) {
                delay(backoff)
                if (!current() || syncState == ConversationSyncState.Live) break
                refreshSnapshot()
                backoff = when (backoff) {
                    2_000L -> 5_000L
                    else -> (backoff * 2).coerceAtMost(30_000L)
                }
            }
        }
    }

    private suspend fun refreshSnapshot(): Boolean = snapshotMutex.withLock {
        if (!current() || syncState == ConversationSyncState.Live) return@withLock false
        val expectedRevision = revision
        val fresh = resetPending
        try {
            val bundle = snapshot(fresh)
            // Reject a delayed fallback when a stream commit/reset arrived while it was loading.
            if (!current() || revision != expectedRevision || syncState == ConversationSyncState.Live) {
                return@withLock false
            }
            cursor = cursorOf(bundle)
            resetPending = false
            onSnapshot(bundle, fresh, false)
            changeState(ConversationSyncState.Cached)
            trace("conversation.snapshot.fallback", null, cursor != null, 0)
            true
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Throwable) {
            if (current()) {
                trace("conversation.snapshot.failed", error, cursor != null, 0)
                if (error.isAuthenticationFailure() || error is SessionScopeChangedException) {
                    stop()
                    onFailure(error)
                }
            }
            false
        }
    }

    fun stop() {
        stopped = true
        revision++
        streamJob?.cancel()
        fallbackJob?.cancel()
        streamJob = null
        fallbackJob = null
        closeStream()
    }
}
