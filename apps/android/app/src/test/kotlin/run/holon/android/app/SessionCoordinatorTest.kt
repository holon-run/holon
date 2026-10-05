package run.holon.android.app

import kotlinx.coroutines.CancellationException
import kotlin.test.*
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonServerInfo

class SessionCoordinatorTest {
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
