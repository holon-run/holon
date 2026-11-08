package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertNotSame
import kotlin.test.assertNull
import kotlin.test.assertSame
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonServerInfo

class AgentAvatarSessionTest {
    private val session = ActiveSession("A", "https://example.test/api/",
        HolonCurrentUser("user", "Alice", "session"), "runtime", "visibility",
        HolonServerInfo("", "local", true, setOf("agent_avatar_v1")))
    private val state = HolonUiState(phase = AppPhase.Ready, session = session)

    @Test fun `only authenticated current session capability enables avatars`() {
        val lease = SessionLease(session, HolonHttpClient(session.baseUrl))
        assertSame(lease, activeAvatarLease(state, lease))
        assertNull(activeAvatarLease(state, null))
        assertNull(activeAvatarLease(state.copy(phase = AppPhase.SignedOut), lease))
        assertNull(activeAvatarLease(state.copy(switchingNetworkId = "B"), lease))
        assertNull(activeAvatarLease(state.copy(session = session.copy(networkId = "B")), lease))
        assertNull(activeAvatarLease(state.copy(session = session.copy(
            server = session.server.copy(capabilities = emptySet()))), lease))
    }

    @Test fun `same scope and client do not make a retired lease current after restore or login`() {
        val sessions = SessionCoordinator()
        val client = HolonHttpClient(session.baseUrl)
        sessions.activate(session, client)
        val old = sessions.capture()
        sessions.activate(session, client)
        assertNotSame(old, activeAvatarLease(state, sessions.current))
        sessions.restore(old)
        assertNotSame(old, activeAvatarLease(state, sessions.current))
        sessions.clear()
        assertNull(activeAvatarLease(state, sessions.current))
    }
}
