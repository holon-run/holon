package run.holon.android.sdk

import run.holon.client.wire.generated.models.AgentListEntry

public data class AgentSummary(
    val id: String,
    val displayName: String,
    val isDefault: Boolean,
    val registryStatus: String,
    val runtimeStatus: String,
    val effectiveModel: String,
    val pending: Int,
    val currentRunId: String?,
    val schedulingPosture: String = "unknown",
    val postureReason: String? = null,
    val waitingReason: String? = null,
    val currentWorkItemId: String? = null,
    val workspaceLabel: String? = null,
    val workspaceId: String? = null,
    val executionRootId: String? = null,
    val workspaceProjectionKind: String? = null,
    val latestBrief: HolonLatestBrief? = null,
)

internal fun AgentListEntry.toAgentSummary(): AgentSummary =
    AgentSummary(
        id = identity.agentId,
        displayName = identity.name ?: identity.agentId,
        isDefault = identity.isDefaultAgent,
        registryStatus = identity.status.value,
        runtimeStatus = status.value,
        effectiveModel = model.effectiveModel,
        pending = pending ?: 0,
        currentRunId = currentRunId,
        schedulingPosture = schedulingPosture?.posture?.value ?: "unknown",
        postureReason = schedulingPosture?.reason,
        waitingReason = waitingReason?.value,
        currentWorkItemId = schedulingPosture?.workItemId,
        workspaceLabel = activeWorkspaceEntry?.workspaceAnchor,
        workspaceId = activeWorkspaceEntry?.workspaceId,
        executionRootId = activeWorkspaceEntry?.executionRootId,
        workspaceProjectionKind = activeWorkspaceEntry?.projectionKind?.value,
    )
