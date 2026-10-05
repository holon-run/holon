package run.holon.android.app

import androidx.lifecycle.SavedStateHandle
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

/** Independent of session startup so a failed resume cannot disable later persistence. */
internal fun observeNavigationBookmarks(scope: CoroutineScope, state: StateFlow<HolonUiState>, handle: SavedStateHandle) =
    scope.launch { state.collect { current -> NavigationBookmark.from(current)?.save(handle) } }

/** Routes contain identity/parameters only; inline expansion is not a route. */
internal sealed interface AppRoute {
    data object Agents : AppRoute
    data object Settings : AppRoute
    data class Agent(val id: String, val section: AgentSection) : AppRoute
    data class Detail(val agentId: String?, val kind: BackTarget) : AppRoute
}

internal fun HolonUiState.route(artifactLoading: Boolean = false): AppRoute {
    val target = backTarget(artifactLoading)
    return when (target) {
        BackTarget.Exit -> AppRoute.Agents
        BackTarget.Agents -> selectedAgent?.let { AppRoute.Agent(it.id, agentSection) } ?: AppRoute.Settings
        BackTarget.Conversation -> AppRoute.Agent(requireNotNull(selectedAgent).id, agentSection)
        else -> AppRoute.Detail(selectedAgent?.id, target)
    }
}

/** No credential, authored content, or cached object graph enters saved instance state. */
internal data class NavigationBookmark(
    val scopeKey: String, val agentId: String?, val section: AgentSection, val destination: MainDestination,
) {
    fun agentFor(state: HolonUiState) =
        state.agents.firstOrNull { state.session?.scopeKey == scopeKey && it.id == agentId }

    fun save(handle: SavedStateHandle) {
        handle["navigation.scope"] = scopeKey
        handle["navigation.agent"] = agentId
        handle["navigation.section"] = section.name
        handle["navigation.destination"] = destination.name
    }

    companion object {
        fun from(state: HolonUiState): NavigationBookmark? =
            state.session?.scopeKey?.takeIf { state.phase == AppPhase.Ready }?.let {
                NavigationBookmark(it, state.selectedAgent?.id, state.agentSection, state.mainDestination)
            }
        fun read(handle: SavedStateHandle): NavigationBookmark? {
            val scope = handle.get<String>("navigation.scope") ?: return null
            return NavigationBookmark(scope, handle["navigation.agent"],
                AgentSection.entries.firstOrNull { it.name == handle.get<String>("navigation.section") } ?: AgentSection.Results,
                MainDestination.entries.firstOrNull { it.name == handle.get<String>("navigation.destination") } ?: MainDestination.Agents)
        }
    }
}

internal enum class BackTarget { Share, Artifact, AddingNetwork, MessageFile, Plan, Activity, FullScreenTurn, Task, WorkItem, Brief, Folder, Conversation, Agents, Exit }

internal fun HolonUiState.backTarget(artifactLoading: Boolean = false): BackTarget = when {
    pendingShare != null -> BackTarget.Share
    artifactLoading -> BackTarget.Artifact
    phase == AppPhase.AddingNetwork -> BackTarget.AddingNetwork
    fileLinkOrigin != null -> BackTarget.MessageFile
    planFile != null -> BackTarget.Plan
    preparedArtifact != null -> BackTarget.Artifact
    hasStandaloneActivity() -> BackTarget.Activity
    fullScreenTurn -> BackTarget.FullScreenTurn
    selectedTask != null -> BackTarget.Task
    selectedWorkItem != null -> BackTarget.WorkItem
    selectedBrief != null -> BackTarget.Brief
    selectedAgent != null && agentSection == AgentSection.Files && !workspaceDirectory?.path.isNullOrBlank() -> BackTarget.Folder
    selectedAgent != null && agentSection != AgentSection.Results -> BackTarget.Conversation
    selectedAgent != null -> BackTarget.Agents
    mainDestination != MainDestination.Agents -> BackTarget.Agents
    else -> BackTarget.Exit
}
