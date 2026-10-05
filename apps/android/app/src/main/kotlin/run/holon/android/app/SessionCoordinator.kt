package run.holon.android.app

import kotlinx.coroutines.CancellationException
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonHttpException

internal data class SessionLease(val session: ActiveSession, val client: HolonHttpClient)

/** The only active session binding. Late responses cannot invalidate its replacement. */
internal class SessionCoordinator {
    @Volatile var current: SessionLease? = null
        private set

    fun activate(session: ActiveSession, client: HolonHttpClient) { current = SessionLease(session, client) }
    fun restore(lease: SessionLease?) { current = lease }
    fun clear() { current = null }
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
