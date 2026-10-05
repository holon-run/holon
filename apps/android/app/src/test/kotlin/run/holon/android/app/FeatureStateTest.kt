package run.holon.android.app

import androidx.lifecycle.SavedStateHandle
import kotlin.test.*

class FeatureStateTest {
    @Test fun `feature update has one owner and preserves other state`() {
        val initial = HolonUiState(draft = "unsent", search = "tester")
        val changed = initial.copy(filesState = initial.filesState.copy(workspaceBusy = true))
        assertEquals("unsent", changed.draft)
        assertEquals("tester", changed.search)
        assertTrue(changed.workspaceBusy)
        assertEquals(changed.filesState, changed.copy(draft = "edited").filesState)
    }

    @Test fun `navigation saves parameters and rejects foreign scope`() {
        val handle = SavedStateHandle()
        val bookmark = NavigationBookmark("A", "holon-tester", AgentSection.Files, MainDestination.Agents)
        bookmark.save(handle)
        assertEquals(bookmark, NavigationBookmark.read(handle))
        assertNull(bookmark.agentFor(HolonUiState()))
        assertEquals(setOf("navigation.scope", "navigation.agent", "navigation.section", "navigation.destination"), handle.keys())
    }

    @Test fun `inline process is not a navigation layer`() {
        assertEquals(BackTarget.Exit, HolonUiState().backTarget())
        assertEquals(AppRoute.Agents, HolonUiState().route())
    }
}
