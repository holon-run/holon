package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlin.test.*
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonServerInfo

class SessionCoordinatorTest {
    @Test fun `foreground metadata refresh preserves browser lease but stale refresh cannot replace a new login`() {
        val owner = SessionCoordinator()
        val identity = session("A")
        val client = HolonHttpClient("https://example.test/api/")
        owner.activate(identity, client)
        val lease = owner.capture()
        val refreshed = identity.copy(user = identity.user.copy(displayName = "Updated"))
        owner.refreshMetadata(lease, refreshed)
        assertSame(lease, owner.capture())
        assertSame(refreshed, lease.session)
        assertSame(lease, owner.state.value)
        owner.activate(identity, client)
        assertFailsWith<CancellationException> { owner.refreshMetadata(lease, refreshed) }
        assertNotSame(lease, owner.capture())
    }

    @Test fun `same identity reactivation and rollback always replace the observable lease`() {
        val owner = SessionCoordinator()
        val identity = session("A")
        val client = HolonHttpClient("https://example.test/api/")
        owner.activate(identity, client)
        val first = owner.capture()
        owner.activate(identity, client)
        val second = owner.capture()
        assertNotSame(first, second)
        assertSame(second, owner.state.value)
        owner.restore(second)
        assertNotSame(second, owner.capture())
        assertSame(owner.capture(), owner.state.value)
    }

    @Test fun `rollback cannot resurrect a retired browser lease`() {
        val owner = SessionCoordinator()
        owner.activate(session("A"), HolonHttpClient("https://example.test/api/"))
        val old = owner.capture()
        owner.clear()
        owner.restore(old)
        assertNotSame(old, owner.capture())
        owner.requireCurrent(old) // Native in-flight requests keep the existing scope/client contract.
    }

    private fun session(network: String) = ActiveSession(network, "https://example.test/api/",
        HolonCurrentUser("user", "Alice", "session"), "runtime", "visibility",
        HolonServerInfo("", "local", true, emptySet()))

    @Test fun `replacement in the same scope rejects late response and old failure`() {
        val owner = SessionCoordinator()
        val first = HolonHttpClient("https://example.test/api/")
        val second = HolonHttpClient("https://example.test/api/")
        owner.activate(session("A"), first)
        val old = owner.capture()
        owner.activate(session("A"), second)
        assertFailsWith<CancellationException> { owner.requireCurrent(old) }
        assertFalse(owner.acceptsFailure(HolonHttpException(401, null, clientInstanceId = first.instanceId)))
        assertFalse(owner.acceptsFailure(SessionScopeChangedException(first.instanceId)))
        assertTrue(owner.acceptsFailure(SessionScopeChangedException(second.instanceId)))
        assertTrue(owner.acceptsFailure(HolonHttpException(401, null, clientInstanceId = second.instanceId)))
        assertSame(second, owner.capture().client)
    }

    @Test fun `clearing a deleted network rejects its lease and late responses`() {
        val owner = SessionCoordinator()
        owner.activate(session("A"), HolonHttpClient("https://example.test/api/"))
        val old = owner.capture()
        assertFailsWith<CancellationException> {
            owner.read(old) {
                owner.clear()
                "late response from deleted network"
            }
        }
        assertEquals(null, owner.current)
        assertFailsWith<CancellationException> { owner.requireCurrent(old) }
    }

    @Test fun `read cannot publish after network switch`() {
        val owner = SessionCoordinator()
        owner.activate(session("A"), HolonHttpClient("https://example.test/api/"))
        assertFailsWith<CancellationException> {
            owner.read {
                owner.activate(session("B"), HolonHttpClient("https://example.test/api/"))
                "late response"
            }
        }
    }
}
