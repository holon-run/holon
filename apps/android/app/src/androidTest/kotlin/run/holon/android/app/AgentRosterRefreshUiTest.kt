package run.holon.android.app

import androidx.activity.compose.setContent
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipeDown
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.util.Collections
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import run.holon.android.sdk.AgentSummary

@RunWith(AndroidJUnit4::class)
class AgentRosterRefreshUiTest {
    @get:Rule val compose = createEmptyComposeRule()
    private val roster = SemanticsMatcher.keyIsDefined(SemanticsProperties.VerticalScrollAxisRange)
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()

    @Test
    fun offlineRetryAndToolbarRefreshIgnoreUnrelatedBusyState() {
        withScreen(HolonUiState(online = false, busy = true)) { actions ->
            compose.onNodeWithText(ui("重试")).assertIsEnabled().performClick()
            compose.waitForIdle()
            assertEquals(listOf(true), actions.refreshRequests.toList())

            compose.onNodeWithContentDescription(ui("刷新")).performClick()
            compose.waitForIdle()
            assertEquals(listOf(true, true), actions.refreshRequests.toList())
        }
    }

    @Test
    fun errorProvidesExplicitRetry() {
        withScreen(HolonUiState(online = true, error = "Roster unavailable")) { actions ->
            compose.onNodeWithText("Roster unavailable").assertExists()
            compose.onNodeWithText(ui("重试")).performClick()
            compose.waitForIdle()
            assertEquals(listOf(true), actions.refreshRequests.toList())
        }
    }

    @Test
    fun emptyListAcceptsPullToRefresh() {
        withScreen(HolonUiState(online = true)) { actions ->
            pullToRefresh()
            assertEquals(listOf(true), actions.refreshRequests.toList())
        }
    }

    @Test
    fun refreshingIgnoresRetryToolbarAndRepeatedPull() {
        withScreen(
            HolonUiState(online = false, error = "Roster unavailable", rosterRefreshing = true),
        ) { actions ->
            compose.onNodeWithText(ui("重试")).assertIsNotEnabled().performClick()
            compose.onNodeWithContentDescription(ui("刷新")).assertIsNotEnabled().performClick()
            pullToRefresh()
            assertTrue(actions.refreshRequests.isEmpty())
        }
    }

    @Test
    fun rosterRemainsScrollableAndRowsRemainClickable() {
        val agents = (0 until 20).map { index ->
            AgentSummary(
                id = "roster-$index",
                displayName = "Roster agent $index",
                isDefault = false,
                registryStatus = "active",
                runtimeStatus = "idle",
                effectiveModel = "test",
                pending = 0,
                currentRunId = null,
            )
        }
        val state = HolonUiState(online = true, agents = agents)
        val lastAgent = state.filteredAgents.last()
        withScreen(state) { actions ->
            compose.onNodeWithText(lastAgent.displayName).assertDoesNotExist()
            compose.onNode(roster).performScrollToNode(hasText(lastAgent.displayName))
            compose.onNodeWithText(lastAgent.displayName).performClick()
            compose.waitForIdle()
            assertEquals(lastAgent.id, actions.openedAgentId)
            assertTrue(actions.refreshRequests.isEmpty())
        }
    }

    private fun pullToRefresh() {
        compose.onNode(roster).performTouchInput { swipeDown() }
        compose.waitForIdle()
    }

    private fun withScreen(
        state: HolonUiState,
        test: (RecordingAgentsActions) -> Unit,
    ) {
        val context = instrumentation.targetContext
        val language = UiCopy.preference()
        UiCopy.select(context, "en")
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                val actions = RecordingAgentsActions()
                scenario.onActivity { activity ->
                    activity.setContent { MaterialTheme { AgentsScreen(state, actions) } }
                }
                compose.onNodeWithContentDescription(ui("刷新")).assertExists()
                if (state.agents.isEmpty()) {
                    compose.onNodeWithText(ui("还没有 Agent")).assertExists()
                }
                test(actions)
            }
        } finally {
            UiCopy.select(context, language)
        }
    }

    private class RecordingAgentsActions : AgentsActions {
        val refreshRequests: MutableList<Boolean> = Collections.synchronizedList(mutableListOf())
        @Volatile var openedAgentId: String? = null

        override fun beginAddNetwork() = Unit
        override fun openAgent(agent: AgentSummary) { openedAgentId = agent.id }
        override fun refresh(showProgress: Boolean) { refreshRequests.add(showProgress) }
        override fun selectMainDestination(destination: MainDestination) = Unit
        override fun setSearch(value: String) = Unit
        override fun switchNetwork(networkId: String) = Unit
    }
}
