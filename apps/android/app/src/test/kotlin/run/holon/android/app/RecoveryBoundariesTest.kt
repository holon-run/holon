package run.holon.android.app

import androidx.lifecycle.SavedStateHandle
import java.io.RandomAccessFile
import java.nio.file.Files
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlin.test.*
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonServerInfo

class RecoveryBoundariesTest {
    @Test fun `cache eviction subtracts deleted bytes and retains newer previews`() {
        val directory = Files.createTempDirectory("holon-preview-eviction").toFile()
        try {
            fun preview(name: String, megabytes: Long, modified: Long) = java.io.File(directory, name).also {
                RandomAccessFile(it, "rw").use { file -> file.setLength(megabytes * 1024 * 1024) }
                it.setLastModified(modified)
            }
            val oldest = preview("oldest", 110, 1)
            val newer = preview("newer", 80, 2)
            val protected = preview("protected", 60, 3)
            trimArtifactCache(directory, protected)
            assertFalse(oldest.exists())
            assertTrue(newer.exists())
            assertTrue(protected.exists())
        } finally { directory.deleteRecursively() }
    }

    @Test fun `bootstrap failure and cancellation both close the opened stream`() = runTest {
        for (failure in listOf(IllegalStateException("bootstrap failed"), CancellationException("cancelled"))) {
            var closed = false
            assertFailsWith<Exception> {
                withOwnedConnection(AutoCloseable { closed = true }) { throw failure }
            }
            assertTrue(closed)
        }
    }

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test fun `navigation is observed after signed out startup recovers`() = runTest {
        val state = MutableStateFlow(HolonUiState(phase = AppPhase.SignedOut, error = "credential store unavailable"))
        val handle = SavedStateHandle()
        val job = observeNavigationBookmarks(this, state, handle)
        runCurrent()
        assertNull(NavigationBookmark.read(handle))
        val session = ActiveSession("A", "https://example.test/api/", HolonCurrentUser("user", "Tester", "session"),
            "runtime", "visibility", HolonServerInfo("", "local", true, emptySet()))
        state.value = state.value.copy(phase = AppPhase.Ready, session = session, agentSection = AgentSection.Files)
        runCurrent()
        assertEquals(AgentSection.Files, NavigationBookmark.read(handle)?.section)
        job.cancel()
    }
}
