package run.holon.android.app

import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonConversationTurn

/** Small foreground navigation cache; durable content still belongs to the repository. */
internal data class ConversationReadingState(
    val snapshot: HolonConversationSnapshot?,
    val olderTurns: List<HolonConversationTurn>,
    val briefs: Map<String, HolonBrief>,
    val before: String?,
    val hasMore: Boolean,
) {
    fun restore(state: HolonUiState, agent: AgentSummary) = state.copy(
        selectedAgent = agent, conversation = snapshot, olderTurns = olderTurns, briefs = briefs,
        historyBeforeCursor = before, hasOlderTurns = hasMore,
        draft = "", attachments = emptyList(), outbox = emptyList(), agentSection = AgentSection.Results,
        selectedBrief = null, selectedTurn = null, fullScreenTurn = false, conversationDetail = null,
        briefOriginWork = null, workOriginBrief = null,
        selectedActivity = null, selectedToolExecution = null, selectedWorkItem = null, planFile = null,
        selectedTask = null, taskOutput = null, tasks = emptyList(), tasksBusy = false, tasksError = null,
        preparedArtifact = null, fileLinkOrigin = null, workItems = emptyList(), workspaces = emptyList(),
        selectedWorkspace = null, workspaceDirectory = null, briefLoads = emptyMap(),
    )

    companion object {
        fun from(state: HolonUiState) = ConversationReadingState(state.conversation, state.olderTurns, state.briefs, state.historyBeforeCursor, state.hasOlderTurns)
    }
}
