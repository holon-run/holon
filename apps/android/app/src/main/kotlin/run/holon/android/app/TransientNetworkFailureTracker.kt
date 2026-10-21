package run.holon.android.app

/**
 * Tracks consecutive transient network failures. A single transport failure (slow server,
 * brief hiccup) keeps the UI online; only 2+ failures in a row flip it offline.
 */
internal class TransientNetworkFailureTracker {
    private var consecutiveFailures = 0

    /** Records one transient transport failure and reports whether the UI should go offline. */
    fun recordFailure(): Boolean {
        consecutiveFailures += 1
        return consecutiveFailures >= OFFLINE_FAILURE_THRESHOLD
    }

    /** Clears the streak after any successful transport (resume ready, roster accept, refresh success). */
    fun reset() {
        consecutiveFailures = 0
    }

    /** Resume-from-offline starts one failure deep: cached content was already served offline. */
    fun markResumedOffline() {
        consecutiveFailures = 1
    }

    companion object {
        const val OFFLINE_FAILURE_THRESHOLD = 2
    }
}
