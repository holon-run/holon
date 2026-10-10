package run.holon.android.app

import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonAgentEvent
import run.holon.android.sdk.HolonContentReportCategory
import run.holon.android.sdk.HolonModelCatalog
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonBriefReadState
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonConversationDetail
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.SseReconnectPolicy
import run.holon.android.sdk.HolonConversationStreamEvent
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonFileReferenceResult
import run.holon.android.sdk.HolonRosterSnapshot
import run.holon.android.sdk.HolonSseConnection
import run.holon.android.sdk.HolonToolExecutionSnapshot
import run.holon.android.sdk.HolonWorkItemSnapshot
import run.holon.android.sdk.HolonTaskSnapshot
import run.holon.android.sdk.HolonTaskOutputSnapshot
import run.holon.android.sdk.HolonWorkspace
import run.holon.android.sdk.HolonWorkspaceDirectory
import run.holon.android.sdk.toConversationEvent

internal enum class AppPhase {
    Starting,
    SignedOut,
    AddingNetwork,
    Ready,
}

internal enum class MainDestination(private val sourceLabel: String) {
    Agents("Agents"),
    Settings("设置"),
    ;

    val label: String get() = ui(sourceLabel)
}

internal enum class AgentSection(private val sourceLabel: String) {
    Results("会话"),
    Work("工作"),
    Files("文件"),
    ;

    val label: String get() = ui(sourceLabel)
}

internal data class FileLinkOrigin(
    val section: AgentSection,
    val brief: HolonBrief?,
    val turn: HolonConversationTurn?,
    val activity: HolonConversationActivity?,
    val workItem: HolonWorkItemSnapshot?,
    val planFile: PreparedArtifact?,
    val workspace: HolonWorkspace?,
    val directory: HolonWorkspaceDirectory?,
)

internal data class ConnectionUiState(
    val phase: AppPhase = AppPhase.Starting,
    val baseUrl: String = "",
    val token: String = "",
    val showToken: Boolean = false,
    val allowInsecureHttp: Boolean = false,
    val pendingPairing: ScannedPairing? = null,
)

internal data class AgentsUiState(
    val agents: List<AgentSummary> = emptyList(),
    val operatorPreviews: Map<String, OperatorPreview?> = emptyMap(),
    val briefReadStates: Map<String, HolonBriefReadState> = emptyMap(),
    val briefReadStatesLoaded: Boolean = false,
    val readBriefIds: Map<String, String> = emptyMap(),
    val readBriefsLoaded: Boolean = false,
    val search: String = "",
)

internal data class ConversationUiState(
    val enqueueing: Boolean = false,
    val stagingAttachment: Boolean = false,
    val abortingRun: Boolean = false,
    val modelCatalog: HolonModelCatalog? = null,
    val modelBusy: Boolean = false,
    val modelError: String? = null,
    val conversation: HolonConversationSnapshot? = null,
    val olderTurns: List<HolonConversationTurn> = emptyList(),
    val historyBeforeCursor: String? = null,
    val hasOlderTurns: Boolean = false,
    val historyBusy: Boolean = false,
    val outbox: List<OutboxEntity> = emptyList(),
    val draft: String = "",
    val attachments: List<StagedAttachment> = emptyList(),
    val briefs: Map<String, HolonBrief> = emptyMap(),
    val briefLoads: Map<String, BriefLoadState> = emptyMap(),
    val selectedBrief: HolonBrief? = null,
    val briefOriginWork: HolonWorkItemSnapshot? = null,
    val workOriginBrief: HolonBrief? = null,
    val selectedTurn: HolonConversationTurn? = null,
    val fullScreenTurn: Boolean = false,
    val conversationDetail: HolonConversationDetail? = null,
    val olderActivitiesBusy: Boolean = false,
    val olderActivitiesLoaded: Boolean = false,
    val selectedActivity: HolonConversationActivity? = null,
    val selectedToolExecution: HolonToolExecutionSnapshot? = null,
    val detailBusy: Boolean = false,
    val reportTarget: ContentReportTarget? = null,
    val reportCategory: HolonContentReportCategory? = null,
    val reportDescription: String = "",
    val reportSubmitting: Boolean = false,
    val reportError: String? = null,
)

internal data class WorkUiState(
    val workItems: List<HolonWorkItemSnapshot> = emptyList(),
    val workItemsLimit: Int = 30,
    val workItemsHasMore: Boolean = false,
    val workItemsLoadingMore: Boolean = false,
    val selectedWorkItem: HolonWorkItemSnapshot? = null,
    val tasks: List<HolonTaskSnapshot> = emptyList(),
    val tasksBusy: Boolean = false,
    val tasksError: String? = null,
    val selectedTask: HolonTaskSnapshot? = null,
    val taskOutput: HolonTaskOutputSnapshot? = null,
    val workItemsBusy: Boolean = false,
)

internal data class FilesUiState(
    val planFile: PreparedArtifact? = null,
    val workspaces: List<HolonWorkspace> = emptyList(),
    val selectedWorkspace: HolonWorkspace? = null,
    val workspaceDirectory: HolonWorkspaceDirectory? = null,
    val workspaceBusy: Boolean = false,
    val preparedArtifact: PreparedArtifact? = null,
    val fileLinkOrigin: FileLinkOrigin? = null,
)

internal data class ShareUiState(
    val pendingShare: PendingAgentShare? = null,
    val queuedShares: List<PendingAgentShare> = emptyList(),
    val shareSending: Boolean = false,
    val shareError: String? = null,
)

internal data class ShellUiState(
    val mainDestination: MainDestination = MainDestination.Agents,
    val busy: Boolean = false,
    val online: Boolean = false,
    val lastSyncedAt: Long? = null,
    val session: ActiveSession? = null,
    val networkProfiles: List<NetworkProfile> = emptyList(),
    val switchingNetworkId: String? = null,
    val selectedAgent: AgentSummary? = null,
    val agentSection: AgentSection = AgentSection.Results,
    val error: String? = null,
    val statusMessage: String? = null,
)

internal data class HolonUiState(
    val connectionState: ConnectionUiState,
    val agentsState: AgentsUiState,
    val conversationState: ConversationUiState,
    val workState: WorkUiState,
    val filesState: FilesUiState,
    val shareState: ShareUiState,
    val shellState: ShellUiState,
) {
    constructor(
        phase: AppPhase = AppPhase.Starting,
        baseUrl: String = "",
        token: String = "",
        showToken: Boolean = false,
        allowInsecureHttp: Boolean = false,
        pendingPairing: ScannedPairing? = null,
        mainDestination: MainDestination = MainDestination.Agents,
        busy: Boolean = false,
        enqueueing: Boolean = false,
        stagingAttachment: Boolean = false,
        abortingRun: Boolean = false,
        online: Boolean = false,
        lastSyncedAt: Long? = null,
        session: ActiveSession? = null,
        networkProfiles: List<NetworkProfile> = emptyList(),
        switchingNetworkId: String? = null,
        agents: List<AgentSummary> = emptyList(),
        operatorPreviews: Map<String, OperatorPreview?> = emptyMap(),
        briefReadStates: Map<String, HolonBriefReadState> = emptyMap(),
        briefReadStatesLoaded: Boolean = false,
        readBriefIds: Map<String, String> = emptyMap(),
        readBriefsLoaded: Boolean = false,
        selectedAgent: AgentSummary? = null,
        modelCatalog: HolonModelCatalog? = null,
        modelBusy: Boolean = false,
        modelError: String? = null,
        conversation: HolonConversationSnapshot? = null,
        olderTurns: List<HolonConversationTurn> = emptyList(),
        historyBeforeCursor: String? = null,
        hasOlderTurns: Boolean = false,
        historyBusy: Boolean = false,
        outbox: List<OutboxEntity> = emptyList(),
        draft: String = "",
        attachments: List<StagedAttachment> = emptyList(),
        pendingShare: PendingAgentShare? = null,
        queuedShares: List<PendingAgentShare> = emptyList(),
        shareSending: Boolean = false,
        shareError: String? = null,
        agentSection: AgentSection = AgentSection.Results,
        briefs: Map<String, HolonBrief> = emptyMap(),
        briefLoads: Map<String, BriefLoadState> = emptyMap(),
        selectedBrief: HolonBrief? = null,
        briefOriginWork: HolonWorkItemSnapshot? = null,
        workOriginBrief: HolonBrief? = null,
        selectedTurn: HolonConversationTurn? = null,
        fullScreenTurn: Boolean = false,
        conversationDetail: HolonConversationDetail? = null,
        olderActivitiesBusy: Boolean = false,
        olderActivitiesLoaded: Boolean = false,
        selectedActivity: HolonConversationActivity? = null,
        selectedToolExecution: HolonToolExecutionSnapshot? = null,
        detailBusy: Boolean = false,
        workItems: List<HolonWorkItemSnapshot> = emptyList(),
        workItemsLimit: Int = 30,
        workItemsHasMore: Boolean = false,
        workItemsLoadingMore: Boolean = false,
        selectedWorkItem: HolonWorkItemSnapshot? = null,
        tasks: List<HolonTaskSnapshot> = emptyList(),
        tasksBusy: Boolean = false,
        tasksError: String? = null,
        selectedTask: HolonTaskSnapshot? = null,
        taskOutput: HolonTaskOutputSnapshot? = null,
        workItemsBusy: Boolean = false,
        planFile: PreparedArtifact? = null,
        workspaces: List<HolonWorkspace> = emptyList(),
        selectedWorkspace: HolonWorkspace? = null,
        workspaceDirectory: HolonWorkspaceDirectory? = null,
        workspaceBusy: Boolean = false,
        preparedArtifact: PreparedArtifact? = null,
        fileLinkOrigin: FileLinkOrigin? = null,
        error: String? = null,
        statusMessage: String? = null,
        search: String = "",
    ) : this(
        connectionState = ConnectionUiState(phase = phase, baseUrl = baseUrl, token = token, showToken = showToken, allowInsecureHttp = allowInsecureHttp, pendingPairing = pendingPairing),
        agentsState = AgentsUiState(agents = agents, operatorPreviews = operatorPreviews, briefReadStates = briefReadStates, briefReadStatesLoaded = briefReadStatesLoaded, readBriefIds = readBriefIds, readBriefsLoaded = readBriefsLoaded, search = search),
        conversationState = ConversationUiState(enqueueing = enqueueing, stagingAttachment = stagingAttachment, abortingRun = abortingRun, modelCatalog = modelCatalog, modelBusy = modelBusy, modelError = modelError, conversation = conversation, olderTurns = olderTurns, historyBeforeCursor = historyBeforeCursor, hasOlderTurns = hasOlderTurns, historyBusy = historyBusy, outbox = outbox, draft = draft, attachments = attachments, briefs = briefs, briefLoads = briefLoads, selectedBrief = selectedBrief, briefOriginWork = briefOriginWork, workOriginBrief = workOriginBrief, selectedTurn = selectedTurn, fullScreenTurn = fullScreenTurn, conversationDetail = conversationDetail, olderActivitiesBusy = olderActivitiesBusy, olderActivitiesLoaded = olderActivitiesLoaded, selectedActivity = selectedActivity, selectedToolExecution = selectedToolExecution, detailBusy = detailBusy),
        workState = WorkUiState(workItems = workItems, workItemsLimit = workItemsLimit, workItemsHasMore = workItemsHasMore, workItemsLoadingMore = workItemsLoadingMore, selectedWorkItem = selectedWorkItem, tasks = tasks, tasksBusy = tasksBusy, tasksError = tasksError, selectedTask = selectedTask, taskOutput = taskOutput, workItemsBusy = workItemsBusy),
        filesState = FilesUiState(planFile = planFile, workspaces = workspaces, selectedWorkspace = selectedWorkspace, workspaceDirectory = workspaceDirectory, workspaceBusy = workspaceBusy, preparedArtifact = preparedArtifact, fileLinkOrigin = fileLinkOrigin),
        shareState = ShareUiState(pendingShare = pendingShare, queuedShares = queuedShares, shareSending = shareSending, shareError = shareError),
        shellState = ShellUiState(mainDestination = mainDestination, busy = busy, online = online, lastSyncedAt = lastSyncedAt, session = session, networkProfiles = networkProfiles, switchingNetworkId = switchingNetworkId, selectedAgent = selectedAgent, agentSection = agentSection, error = error, statusMessage = statusMessage),
    )

    // Transitional shell adapter; feature state has a single immutable owner.
    val phase: AppPhase get() = connectionState.phase
    val baseUrl: String get() = connectionState.baseUrl
    val token: String get() = connectionState.token
    val showToken: Boolean get() = connectionState.showToken
    val allowInsecureHttp: Boolean get() = connectionState.allowInsecureHttp
    val pendingPairing: ScannedPairing? get() = connectionState.pendingPairing
    val mainDestination: MainDestination get() = shellState.mainDestination
    val busy: Boolean get() = shellState.busy
    val enqueueing: Boolean get() = conversationState.enqueueing
    val stagingAttachment: Boolean get() = conversationState.stagingAttachment
    val abortingRun: Boolean get() = conversationState.abortingRun
    val online: Boolean get() = shellState.online
    val lastSyncedAt: Long? get() = shellState.lastSyncedAt
    val session: ActiveSession? get() = shellState.session
    val networkProfiles: List<NetworkProfile> get() = shellState.networkProfiles
    val switchingNetworkId: String? get() = shellState.switchingNetworkId
    val agents: List<AgentSummary> get() = agentsState.agents
    val operatorPreviews: Map<String, OperatorPreview?> get() = agentsState.operatorPreviews
    val briefReadStates: Map<String, HolonBriefReadState> get() = agentsState.briefReadStates
    val briefReadStatesLoaded: Boolean get() = agentsState.briefReadStatesLoaded
    val readBriefIds: Map<String, String> get() = agentsState.readBriefIds
    val readBriefsLoaded: Boolean get() = agentsState.readBriefsLoaded
    val selectedAgent: AgentSummary? get() = shellState.selectedAgent
    val modelCatalog: HolonModelCatalog? get() = conversationState.modelCatalog
    val modelBusy: Boolean get() = conversationState.modelBusy
    val modelError: String? get() = conversationState.modelError
    val conversation: HolonConversationSnapshot? get() = conversationState.conversation
    val olderTurns: List<HolonConversationTurn> get() = conversationState.olderTurns
    val historyBeforeCursor: String? get() = conversationState.historyBeforeCursor
    val hasOlderTurns: Boolean get() = conversationState.hasOlderTurns
    val historyBusy: Boolean get() = conversationState.historyBusy
    val outbox: List<OutboxEntity> get() = conversationState.outbox
    val draft: String get() = conversationState.draft
    val attachments: List<StagedAttachment> get() = conversationState.attachments
    val pendingShare: PendingAgentShare? get() = shareState.pendingShare
    val queuedShares: List<PendingAgentShare> get() = shareState.queuedShares
    val shareSending: Boolean get() = shareState.shareSending
    val shareError: String? get() = shareState.shareError
    val agentSection: AgentSection get() = shellState.agentSection
    val briefs: Map<String, HolonBrief> get() = conversationState.briefs
    val briefLoads: Map<String, BriefLoadState> get() = conversationState.briefLoads
    val selectedBrief: HolonBrief? get() = conversationState.selectedBrief
    val briefOriginWork: HolonWorkItemSnapshot? get() = conversationState.briefOriginWork
    val workOriginBrief: HolonBrief? get() = conversationState.workOriginBrief
    val selectedTurn: HolonConversationTurn? get() = conversationState.selectedTurn
    val fullScreenTurn: Boolean get() = conversationState.fullScreenTurn
    val conversationDetail: HolonConversationDetail? get() = conversationState.conversationDetail
    val olderActivitiesBusy: Boolean get() = conversationState.olderActivitiesBusy
    val olderActivitiesLoaded: Boolean get() = conversationState.olderActivitiesLoaded
    val selectedActivity: HolonConversationActivity? get() = conversationState.selectedActivity
    val selectedToolExecution: HolonToolExecutionSnapshot? get() = conversationState.selectedToolExecution
    val detailBusy: Boolean get() = conversationState.detailBusy
    val reportTarget: ContentReportTarget? get() = conversationState.reportTarget
    val reportCategory: HolonContentReportCategory? get() = conversationState.reportCategory
    val reportDescription: String get() = conversationState.reportDescription
    val reportSubmitting: Boolean get() = conversationState.reportSubmitting
    val reportError: String? get() = conversationState.reportError
    val workItems: List<HolonWorkItemSnapshot> get() = workState.workItems
    val workItemsLimit: Int get() = workState.workItemsLimit
    val workItemsHasMore: Boolean get() = workState.workItemsHasMore
    val workItemsLoadingMore: Boolean get() = workState.workItemsLoadingMore
    val selectedWorkItem: HolonWorkItemSnapshot? get() = workState.selectedWorkItem
    val tasks: List<HolonTaskSnapshot> get() = workState.tasks
    val tasksBusy: Boolean get() = workState.tasksBusy
    val tasksError: String? get() = workState.tasksError
    val selectedTask: HolonTaskSnapshot? get() = workState.selectedTask
    val taskOutput: HolonTaskOutputSnapshot? get() = workState.taskOutput
    val workItemsBusy: Boolean get() = workState.workItemsBusy
    val planFile: PreparedArtifact? get() = filesState.planFile
    val workspaces: List<HolonWorkspace> get() = filesState.workspaces
    val selectedWorkspace: HolonWorkspace? get() = filesState.selectedWorkspace
    val workspaceDirectory: HolonWorkspaceDirectory? get() = filesState.workspaceDirectory
    val workspaceBusy: Boolean get() = filesState.workspaceBusy
    val preparedArtifact: PreparedArtifact? get() = filesState.preparedArtifact
    val fileLinkOrigin: FileLinkOrigin? get() = filesState.fileLinkOrigin
    val error: String? get() = shellState.error
    val statusMessage: String? get() = shellState.statusMessage
    val search: String get() = agentsState.search

    fun copy(
        phase: AppPhase = this.phase,
        baseUrl: String = this.baseUrl,
        token: String = this.token,
        showToken: Boolean = this.showToken,
        allowInsecureHttp: Boolean = this.allowInsecureHttp,
        pendingPairing: ScannedPairing? = this.pendingPairing,
        mainDestination: MainDestination = this.mainDestination,
        busy: Boolean = this.busy,
        enqueueing: Boolean = this.enqueueing,
        stagingAttachment: Boolean = this.stagingAttachment,
        abortingRun: Boolean = this.abortingRun,
        online: Boolean = this.online,
        lastSyncedAt: Long? = this.lastSyncedAt,
        session: ActiveSession? = this.session,
        networkProfiles: List<NetworkProfile> = this.networkProfiles,
        switchingNetworkId: String? = this.switchingNetworkId,
        agents: List<AgentSummary> = this.agents,
        operatorPreviews: Map<String, OperatorPreview?> = this.operatorPreviews,
        briefReadStates: Map<String, HolonBriefReadState> = this.briefReadStates,
        briefReadStatesLoaded: Boolean = this.briefReadStatesLoaded,
        readBriefIds: Map<String, String> = this.readBriefIds,
        readBriefsLoaded: Boolean = this.readBriefsLoaded,
        selectedAgent: AgentSummary? = this.selectedAgent,
        modelCatalog: HolonModelCatalog? = this.modelCatalog,
        modelBusy: Boolean = this.modelBusy,
        modelError: String? = this.modelError,
        conversation: HolonConversationSnapshot? = this.conversation,
        olderTurns: List<HolonConversationTurn> = this.olderTurns,
        historyBeforeCursor: String? = this.historyBeforeCursor,
        hasOlderTurns: Boolean = this.hasOlderTurns,
        historyBusy: Boolean = this.historyBusy,
        outbox: List<OutboxEntity> = this.outbox,
        draft: String = this.draft,
        attachments: List<StagedAttachment> = this.attachments,
        pendingShare: PendingAgentShare? = this.pendingShare,
        queuedShares: List<PendingAgentShare> = this.queuedShares,
        shareSending: Boolean = this.shareSending,
        shareError: String? = this.shareError,
        agentSection: AgentSection = this.agentSection,
        briefs: Map<String, HolonBrief> = this.briefs,
        briefLoads: Map<String, BriefLoadState> = this.briefLoads,
        selectedBrief: HolonBrief? = this.selectedBrief,
        briefOriginWork: HolonWorkItemSnapshot? = this.briefOriginWork,
        workOriginBrief: HolonBrief? = this.workOriginBrief,
        selectedTurn: HolonConversationTurn? = this.selectedTurn,
        fullScreenTurn: Boolean = this.fullScreenTurn,
        conversationDetail: HolonConversationDetail? = this.conversationDetail,
        olderActivitiesBusy: Boolean = this.olderActivitiesBusy,
        olderActivitiesLoaded: Boolean = this.olderActivitiesLoaded,
        selectedActivity: HolonConversationActivity? = this.selectedActivity,
        selectedToolExecution: HolonToolExecutionSnapshot? = this.selectedToolExecution,
        detailBusy: Boolean = this.detailBusy,
        reportTarget: ContentReportTarget? = this.reportTarget,
        reportCategory: HolonContentReportCategory? = this.reportCategory,
        reportDescription: String = this.reportDescription,
        reportSubmitting: Boolean = this.reportSubmitting,
        reportError: String? = this.reportError,
        workItems: List<HolonWorkItemSnapshot> = this.workItems,
        workItemsLimit: Int = this.workItemsLimit,
        workItemsHasMore: Boolean = this.workItemsHasMore,
        workItemsLoadingMore: Boolean = this.workItemsLoadingMore,
        selectedWorkItem: HolonWorkItemSnapshot? = this.selectedWorkItem,
        tasks: List<HolonTaskSnapshot> = this.tasks,
        tasksBusy: Boolean = this.tasksBusy,
        tasksError: String? = this.tasksError,
        selectedTask: HolonTaskSnapshot? = this.selectedTask,
        taskOutput: HolonTaskOutputSnapshot? = this.taskOutput,
        workItemsBusy: Boolean = this.workItemsBusy,
        planFile: PreparedArtifact? = this.planFile,
        workspaces: List<HolonWorkspace> = this.workspaces,
        selectedWorkspace: HolonWorkspace? = this.selectedWorkspace,
        workspaceDirectory: HolonWorkspaceDirectory? = this.workspaceDirectory,
        workspaceBusy: Boolean = this.workspaceBusy,
        preparedArtifact: PreparedArtifact? = this.preparedArtifact,
        fileLinkOrigin: FileLinkOrigin? = this.fileLinkOrigin,
        error: String? = this.error,
        statusMessage: String? = this.statusMessage,
        search: String = this.search,
    ): HolonUiState = HolonUiState(
        connectionState = ConnectionUiState(phase = phase, baseUrl = baseUrl, token = token, showToken = showToken, allowInsecureHttp = allowInsecureHttp, pendingPairing = pendingPairing),
        agentsState = AgentsUiState(agents = agents, operatorPreviews = operatorPreviews, briefReadStates = briefReadStates, briefReadStatesLoaded = briefReadStatesLoaded, readBriefIds = readBriefIds, readBriefsLoaded = readBriefsLoaded, search = search),
        conversationState = ConversationUiState(enqueueing = enqueueing, stagingAttachment = stagingAttachment, abortingRun = abortingRun, modelCatalog = modelCatalog, modelBusy = modelBusy, modelError = modelError, conversation = conversation, olderTurns = olderTurns, historyBeforeCursor = historyBeforeCursor, hasOlderTurns = hasOlderTurns, historyBusy = historyBusy, outbox = outbox, draft = draft, attachments = attachments, briefs = briefs, briefLoads = briefLoads, selectedBrief = selectedBrief, briefOriginWork = briefOriginWork, workOriginBrief = workOriginBrief, selectedTurn = selectedTurn, fullScreenTurn = fullScreenTurn, conversationDetail = conversationDetail, olderActivitiesBusy = olderActivitiesBusy, olderActivitiesLoaded = olderActivitiesLoaded, selectedActivity = selectedActivity, selectedToolExecution = selectedToolExecution, detailBusy = detailBusy, reportTarget = reportTarget, reportCategory = reportCategory, reportDescription = reportDescription, reportSubmitting = reportSubmitting, reportError = reportError),
        workState = WorkUiState(workItems = workItems, workItemsLimit = workItemsLimit, workItemsHasMore = workItemsHasMore, workItemsLoadingMore = workItemsLoadingMore, selectedWorkItem = selectedWorkItem, tasks = tasks, tasksBusy = tasksBusy, tasksError = tasksError, selectedTask = selectedTask, taskOutput = taskOutput, workItemsBusy = workItemsBusy),
        filesState = FilesUiState(planFile = planFile, workspaces = workspaces, selectedWorkspace = selectedWorkspace, workspaceDirectory = workspaceDirectory, workspaceBusy = workspaceBusy, preparedArtifact = preparedArtifact, fileLinkOrigin = fileLinkOrigin),
        shareState = ShareUiState(pendingShare = pendingShare, queuedShares = queuedShares, shareSending = shareSending, shareError = shareError),
        shellState = ShellUiState(mainDestination = mainDestination, busy = busy, online = online, lastSyncedAt = lastSyncedAt, session = session, networkProfiles = networkProfiles, switchingNetworkId = switchingNetworkId, selectedAgent = selectedAgent, agentSection = agentSection, error = error, statusMessage = statusMessage),
    )

    val recentAgents: List<AgentSummary>
        get() =
            agents.sortedWith(
                compareByDescending<AgentSummary> { it.needsReply() }
                    .thenByDescending {
                        if (briefReadStatesLoaded) {
                            it.unreadCount(briefReadStates) > 0
                        } else {
                            readBriefsLoaded && it.hasUnreadBrief(readBriefIds)
                        }
                    }
                    .thenByDescending { listOfNotNull(activityTime(it.latestBrief?.createdAt), activityTime(operatorPreviews[it.id]?.createdAt)).maxOrNull() }
                    .thenBy { it.displayName.lowercase() },
            )

    val filteredAgents: List<AgentSummary>
        get() =
            recentAgents.filter {
                search.isBlank() ||
                    it.displayName.contains(search, ignoreCase = true) ||
                    it.id.contains(search, ignoreCase = true)
            }
}

internal fun HolonUiState.forAddingNetwork(): HolonUiState =
    copy(
        phase = AppPhase.AddingNetwork,
        baseUrl = "",
        token = "",
        showToken = false,
        allowInsecureHttp = false,
        error = null,
        statusMessage = null,
    )

internal fun HolonUiState.afterCancelAddingNetwork(): HolonUiState =
    copy(
        phase = AppPhase.Ready,
        baseUrl = session?.baseUrl.orEmpty(),
        token = "",
        showToken = false,
        allowInsecureHttp = networkProfiles.firstOrNull { it.networkId == session?.networkId }?.allowInsecureHttp ?: false,
        error = null,
        statusMessage = null,
    )

internal fun HolonUiState.canDeleteNetwork(networkId: String): Boolean =
    phase in setOf(AppPhase.Ready, AppPhase.SignedOut) &&
        !busy && !enqueueing && !stagingAttachment &&
        networkProfiles.any { it.networkId == networkId }

internal fun HolonUiState.afterDeletingNetwork(
    networkId: String,
    profiles: List<NetworkProfile>,
    sessionIsCurrent: Boolean = true,
): HolonUiState =
    // Saved profiles remain authoritative even after a session transition.
    if (!sessionIsCurrent) {
        copy(networkProfiles = profiles)
    } else if (session?.networkId == networkId ||
        (session == null && networkProfiles.firstOrNull { it.networkId == networkId }?.baseUrl == baseUrl)
    ) {
        HolonUiState(phase = AppPhase.SignedOut, networkProfiles = profiles, statusMessage = "网络已删除")
    } else {
        copy(busy = false, networkProfiles = profiles, error = null, statusMessage = "网络已删除")
    }

internal fun isCurrentLiveSync(
    foreground: Boolean,
    phase: AppPhase,
    expectedGeneration: Long,
    currentGeneration: Long,
): Boolean =
    foreground && phase == AppPhase.Ready && expectedGeneration == currentGeneration
