package run.holon.android.app

import java.time.Instant
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonModelOption

internal data class OperatorPreview(val text: String, val createdAt: String?)

/** Only operator-visible input from the newest turn, never internal/tool text. */
internal fun HolonConversationSnapshot.operatorPreview(): OperatorPreview? {
    val turn = turns.lastOrNull() ?: return null
    if (turn.briefIds.isNotEmpty()) return null
    val input = turn.inputs.lastOrNull {
        it.presentationClass == "operator" || (it.presentationClass == null && turn.presentationClass == "operator")
    } ?: return null
    return input.preview.takeIf(String::isNotBlank)?.let { OperatorPreview(it, input.createdAt ?: turn.startedAt) }
}

internal fun activityTime(value: String?): Instant? = value?.let { runCatching { Instant.parse(it) }.getOrNull() }

internal fun AgentSummary.inputPreview(preview: OperatorPreview?): String? {
    val brief = latestBrief
    return preview?.takeIf {
        brief == null || (activityTime(it.createdAt)?.let { inputAt ->
            activityTime(brief.createdAt)?.let { briefAt -> inputAt > briefAt }
        } == true)
    }?.text
}

internal fun commonModelOptions(options: List<HolonModelOption>, agents: List<AgentSummary>, currentModel: String): List<HolonModelOption> {
    val usage = agents.map { it.effectiveModel }.filter(String::isNotBlank).groupingBy { it }.eachCount()
    return options.filter { it.model in usage || it.model == currentModel }
        .sortedWith(compareByDescending<HolonModelOption> { it.model == currentModel }
            .thenByDescending { usage[it.model] ?: 0 }.thenBy { it.displayName.lowercase() })
}

internal fun HolonUiState.hasStandaloneActivity(): Boolean =
    selectedActivity != null && (fullScreenTurn || selectedTurn == null) &&
        selectedWorkItem == null && selectedBrief == null && agentSection == AgentSection.Results

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
