package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonHttpException

// A lease is an incarnation, even when the identity and client are unchanged.
internal class SessionLease(session: ActiveSession, val client: HolonHttpClient) {
    @Volatile var session: ActiveSession = session
        private set

    fun refreshMetadata(updated: ActiveSession) {
        require(updated.scopeKey == session.scopeKey && updated.baseUrl == session.baseUrl)
        session = updated
    }
}

/** The only active session binding. Late responses cannot invalidate its replacement. */
internal class SessionCoordinator {
    private val binding = MutableStateFlow<SessionLease?>(null)
    val state = binding.asStateFlow()
    val current: SessionLease? get() = binding.value

    fun activate(session: ActiveSession, client: HolonHttpClient) { binding.value = SessionLease(session, client) }
    fun refreshMetadata(lease: SessionLease, session: ActiveSession) {
        if (current !== lease) throw CancellationException("Stale session refresh")
        lease.refreshMetadata(session)
    }
    // A rollback restores the identity, not a retired browser lease.
    fun restore(lease: SessionLease?) { binding.value = lease?.let { SessionLease(it.session, it.client) } }
    fun clear() { binding.value = null }
    fun capture(): SessionLease = checkNotNull(current) { "No active Holon session" }

    fun requireCurrent(lease: SessionLease) {
        val active = current
        if (active?.session?.scopeKey != lease.session.scopeKey || active.client !== lease.client) {
            throw CancellationException("Stale session response")
        }
    }

    fun acceptsFailure(error: Throwable): Boolean {
        val clientId = when (error) {
            is HolonHttpException -> error.clientInstanceId
            is SessionScopeChangedException -> error.clientInstanceId
            else -> null
        }
        return clientId == null || clientId == current?.client?.instanceId
    }

    fun <T> read(read: (HolonHttpClient) -> T): T {
        val lease = capture()
        return read(lease, read)
    }

    fun <T> read(lease: SessionLease, read: (HolonHttpClient) -> T): T {
        requireCurrent(lease)
        return read(lease.client).also { requireCurrent(lease) }
    }
}
