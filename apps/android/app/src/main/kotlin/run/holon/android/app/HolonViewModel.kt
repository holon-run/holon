package run.holon.android.app

import android.app.Application
import android.content.Context
import android.net.Uri
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonAgentEvent
import run.holon.android.sdk.HolonBrief
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
import run.holon.android.sdk.HolonWorkspace
import run.holon.android.sdk.HolonWorkspaceDirectory
import run.holon.android.sdk.toConversationEvent

internal enum class AppPhase {
    Starting,
    SignedOut,
    Ready,
}

internal enum class MainDestination(val label: String) {
    Agents("Agents"),
    Settings("设置"),
}

internal enum class AgentSection(val label: String) {
    Results("结果"),
    Work("工作"),
    Files("文件"),
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

internal data class HolonUiState(
    val phase: AppPhase = AppPhase.Starting,
    val baseUrl: String = "",
    val token: String = "",
    val showToken: Boolean = false,
    val allowInsecureHttp: Boolean = false,
    val mainDestination: MainDestination = MainDestination.Agents,
    val busy: Boolean = false,
    val enqueueing: Boolean = false,
    val stagingAttachment: Boolean = false,
    val abortingRun: Boolean = false,
    val online: Boolean = false,
    val lastSyncedAt: Long? = null,
    val session: ActiveSession? = null,
    val agents: List<AgentSummary> = emptyList(),
    val readBriefIds: Map<String, String> = emptyMap(),
    val readBriefsLoaded: Boolean = false,
    val selectedAgent: AgentSummary? = null,
    val conversation: HolonConversationSnapshot? = null,
    val olderTurns: List<HolonConversationTurn> = emptyList(),
    val historyBeforeCursor: String? = null,
    val hasOlderTurns: Boolean = false,
    val historyBusy: Boolean = false,
    val outbox: List<OutboxEntity> = emptyList(),
    val draft: String = "",
    val attachments: List<StagedAttachment> = emptyList(),
    val agentSection: AgentSection = AgentSection.Results,
    val briefs: Map<String, HolonBrief> = emptyMap(),
    val selectedBrief: HolonBrief? = null,
    val selectedTurn: HolonConversationTurn? = null,
    val conversationDetail: HolonConversationDetail? = null,
    val olderActivitiesBusy: Boolean = false,
    val olderActivitiesLoaded: Boolean = false,
    val selectedActivity: HolonConversationActivity? = null,
    val selectedToolExecution: HolonToolExecutionSnapshot? = null,
    val detailBusy: Boolean = false,
    val workItems: List<HolonWorkItemSnapshot> = emptyList(),
    val workItemsLimit: Int = 30,
    val workItemsHasMore: Boolean = false,
    val workItemsLoadingMore: Boolean = false,
    val selectedWorkItem: HolonWorkItemSnapshot? = null,
    val workItemsBusy: Boolean = false,
    val planFile: PreparedArtifact? = null,
    val workspaces: List<HolonWorkspace> = emptyList(),
    val selectedWorkspace: HolonWorkspace? = null,
    val workspaceDirectory: HolonWorkspaceDirectory? = null,
    val workspaceBusy: Boolean = false,
    val preparedArtifact: PreparedArtifact? = null,
    val fileLinkOrigin: FileLinkOrigin? = null,
    val error: String? = null,
    val statusMessage: String? = null,
    val search: String = "",
) {
    val recentAgents: List<AgentSummary>
        get() =
            agents.sortedWith(
                compareByDescending<AgentSummary> { it.needsReply() }
                    .thenByDescending { readBriefsLoaded && it.hasUnreadBrief(readBriefIds) }
                    .thenByDescending { it.latestBrief?.createdAt.orEmpty() }
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

internal class AppContainer(context: Context) {
    private val database = HolonDatabase.create(context)
    val repository =
        HolonRepository(
            context = context,
            sessionStore = createSessionStore(context),
            preferences = HostPreferences(context),
            dao = database.holonDao(),
        )
}

internal class HolonViewModel(
    application: Application,
    private val repository: HolonRepository,
) : AndroidViewModel(application) {
    private val mutableState = MutableStateFlow(HolonUiState(baseUrl = defaultBaseUrl()))
    val state: StateFlow<HolonUiState> = mutableState.asStateFlow()
    private var conversationJob: Job? = null
    private var conversationStreamJob: Job? = null
    private var conversationStream: HolonSseConnection? = null
    private var globalEventStreamJob: Job? = null
    private val agentEventStreamJobs = ConcurrentHashMap<String, Job>()
    private val agentEventCursors = ConcurrentHashMap<String, Long>()
    private var liveRosterRefreshJob: Job? = null
    private var detailRefreshJob: Job? = null
    private var workspaceBrowseJob: Job? = null
    private var workspaceBrowseGeneration = 0L
    private var workspaceBrowseRequest: WorkspaceBrowseRequest? = null
    private var refreshJob: Job? = null
    @Volatile private var foreground = true
    private val draftSaveJobs = mutableMapOf<String, Job>()
    private val composerSaveJobs = mutableMapOf<String, Job>()
    private var draftRevision = 0L

    init {
        viewModelScope.launch {
            val result = withContext(Dispatchers.IO) { repository.resume() }
            when (result) {
                ResumeResult.NoSession ->
                    mutableState.update { it.copy(phase = AppPhase.SignedOut) }
                is ResumeResult.Ready -> {
                    mutableState.update {
                        it.copy(
                            phase = AppPhase.Ready,
                            online = true,
                            lastSyncedAt = System.currentTimeMillis(),
                            session = result.session,
                            baseUrl = result.session.baseUrl,
                            agents = result.roster.agents,
                        )
                    }
                    loadReadBriefIds()
                    startLiveSync(result.roster.agents)
                    viewModelScope.launch(Dispatchers.IO) { runCatching { repository.retryOutbox() } }
                }
                is ResumeResult.Offline -> {
                    mutableState.update {
                        it.copy(
                            phase = AppPhase.Ready,
                            online = false,
                            session = result.session,
                            baseUrl = result.session.baseUrl,
                            agents = result.cached.map(ConversationCacheEntity::toAgentSummary),
                            statusMessage = "当前离线，显示上次同步内容",
                        )
                    }
                    loadReadBriefIds()
                    refresh(showProgress = false)
                }
                is ResumeResult.Incompatible ->
                    mutableState.update {
                        it.copy(
                            phase = AppPhase.SignedOut,
                            baseUrl = result.baseUrl,
                            error = result.message,
                        )
                    }
            }
        }
    }

    fun setBaseUrl(value: String) =
        mutableState.update {
            it.copy(baseUrl = value, allowInsecureHttp = false, error = null)
        }
    fun setToken(value: String) = mutableState.update { it.copy(token = value, error = null) }
    fun toggleToken() = mutableState.update { it.copy(showToken = !it.showToken) }
    fun setAllowInsecureHttp(value: Boolean) =
        mutableState.update { it.copy(allowInsecureHttp = value, error = null) }
    fun setSearch(value: String) = mutableState.update { it.copy(search = value) }

    private fun loadReadBriefIds() {
        val scopeKey = state.value.session?.scopeKey ?: return
        val agents = state.value.agents
        viewModelScope.launch {
            runCatching { withContext(Dispatchers.IO) { repository.readBriefIds(agents) } }
                .onSuccess { read ->
                    if (state.value.session?.scopeKey == scopeKey) {
                        mutableState.update { it.copy(readBriefIds = read, readBriefsLoaded = true) }
                    }
                }
        }
    }

    fun markBriefRead(agentId: String, briefId: String) {
        val current = state.value
        if (current.selectedAgent?.id != agentId || current.readBriefIds[agentId] == briefId) return
        val scopeKey = current.session?.scopeKey ?: return
        mutableState.update { it.copy(readBriefIds = it.readBriefIds + (agentId to briefId)) }
        viewModelScope.launch {
            runCatching { withContext(Dispatchers.IO) { repository.markBriefRead(agentId, briefId) } }
                .onFailure { error ->
                    if (state.value.session?.scopeKey == scopeKey) {
                        mutableState.update { it.copy(error = "无法保存已读状态：${humanError(error)}") }
                    }
                }
        }
    }

    fun login() {
        val before = state.value
        if (before.busy) return
        if (before.token.isBlank()) {
            mutableState.update { it.copy(error = "请输入 token") }
            return
        }
        val tokenChars = before.token.toCharArray()
        mutableState.update { it.copy(busy = true, error = null, statusMessage = "正在安全登录…") }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    repository.login(before.baseUrl, tokenChars, before.allowInsecureHttp)
                }
            }.onSuccess { (session, roster) ->
                mutableState.update {
                    it.copy(
                        phase = AppPhase.Ready,
                        busy = false,
                        online = true,
                        lastSyncedAt = System.currentTimeMillis(),
                        session = session,
                        baseUrl = session.baseUrl,
                        token = "",
                        agents = roster.agents,
                        readBriefIds = emptyMap(),
                        readBriefsLoaded = false,
                        statusMessage = null,
                    )
                }
                loadReadBriefIds()
                startLiveSync(roster.agents)
            }.onFailure { error ->
                tokenChars.fill('\u0000')
                mutableState.update {
                    it.copy(busy = false, token = "", error = humanError(error), statusMessage = null)
                }
            }
        }
    }

    fun onForeground() {
        foreground = true
        if (state.value.phase != AppPhase.Ready || state.value.busy) return
        startLiveSync(state.value.agents)
        refresh(showProgress = false)
    }

    fun onBackground() {
        foreground = false
        stopConversationStream()
        stopLiveSync()
    }

    fun refresh(showProgress: Boolean = true) {
        if (state.value.phase != AppPhase.Ready) return
        if (refreshJob?.isActive == true) return
        if (showProgress) mutableState.update { it.copy(busy = true, error = null) }
        refreshJob = viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    repository.refreshSessionAndRoster()
                }
            }.onSuccess { (session, roster) ->
                mutableState.update {
                    it.copy(
                        session = session,
                        agents = roster.agents,
                        online = true,
                        lastSyncedAt = System.currentTimeMillis(),
                        busy = false,
                        statusMessage = null,
                    )
                }
                startLiveSync(roster.agents)
                state.value.selectedAgent?.let(::openAgent)
                viewModelScope.launch {
                    runCatching {
                        withContext(Dispatchers.IO) { repository.retryOutbox() }
                    }.onSuccess {
                        state.value.selectedAgent?.id?.let { agentId ->
                            runCatching { withContext(Dispatchers.IO) { repository.outbox(agentId) } }
                                .onSuccess { messages ->
                                    if (state.value.selectedAgent?.id == agentId) {
                                        mutableState.update { it.copy(outbox = messages) }
                                    }
                                }
                        }
                    }.onFailure(::handleRuntimeFailure)
                }
            }.onFailure { error ->
                handleRuntimeFailure(error)
                if (state.value.phase == AppPhase.Ready && foreground) {
                    state.value.selectedAgent?.let { agent ->
                        startConversationStream(agent, state.value.conversation?.snapshotCursor)
                    }
                }
            }
        }
    }

    private fun startLiveSync(agents: List<AgentSummary>) {
        if (!foreground || state.value.phase != AppPhase.Ready) return
        if (globalEventStreamJob?.isActive != true) {
            globalEventStreamJob =
                viewModelScope.launch(Dispatchers.IO) {
                    runCatching {
                        while (isActive && foreground) {
                            repository.reconnectingRosterHints(
                                policy = SseReconnectPolicy(maxAttempts = 8),
                            ).forEach {
                                if (isActive && foreground) scheduleLiveRosterRefresh()
                            }
                            delay(500)
                        }
                    }.onFailure { error ->
                        if (isActive && foreground) {
                            mutableState.update {
                                it.copy(statusMessage = "列表同步已暂停：${humanError(error)}")
                            }
                        }
                    }
                }
        }
        val ids = agents.mapTo(mutableSetOf()) { it.id }
        agentEventStreamJobs.keys.toList()
            .filter { it !in ids }
            .forEach { agentId ->
                agentEventStreamJobs.remove(agentId)?.cancel()
                agentEventCursors.remove(agentId)
            }
        agents.forEach { agent ->
            if (agentEventStreamJobs[agent.id]?.isActive == true) return@forEach
            agentEventStreamJobs[agent.id] =
                viewModelScope.launch(Dispatchers.IO) {
                    runCatching {
                        while (isActive && foreground) {
                            repository.reconnectingAgentEvents(
                                agentId = agent.id,
                                afterSeq = agentEventCursors[agent.id],
                                policy = SseReconnectPolicy(maxAttempts = 8),
                            ).forEach { event ->
                                if (!isActive || !foreground) return@forEach
                                agentEventCursors[agent.id] = event.eventSeq
                                scheduleLiveRosterRefresh()
                            }
                            delay(500)
                        }
                    }.onFailure { error ->
                        if (isActive && foreground) {
                            mutableState.update {
                                it.copy(statusMessage = "列表同步已暂停：${humanError(error)}")
                            }
                        }
                    }
                }
        }
    }

    private fun stopLiveSync() {
        globalEventStreamJob?.cancel()
        globalEventStreamJob = null
        agentEventStreamJobs.values.forEach(Job::cancel)
        agentEventStreamJobs.clear()
        liveRosterRefreshJob?.cancel()
        liveRosterRefreshJob = null
    }

    private fun scheduleLiveRosterRefresh() {
        if (!foreground || state.value.phase != AppPhase.Ready) return
        liveRosterRefreshJob?.cancel()
        liveRosterRefreshJob =
            viewModelScope.launch {
                delay(250)
                runCatching {
                    withContext(Dispatchers.IO) { repository.refreshSessionAndRoster() }
                }.onSuccess { (session, roster) ->
                    if (!foreground || state.value.phase != AppPhase.Ready) return@onSuccess
                    mutableState.update {
                        it.copy(
                            session = session,
                            agents = roster.agents,
                            online = true,
                            lastSyncedAt = System.currentTimeMillis(),
                            statusMessage = null,
                        )
                    }
                    loadReadBriefIds()
                    startLiveSync(roster.agents)
                }.onFailure(::handleRuntimeFailure)
            }
    }

    fun openAgent(agent: AgentSummary) {
        if (state.value.enqueueing) return
        val sameAgentBeforeLoad = state.value.selectedAgent?.id == agent.id
        conversationJob?.cancel()
        stopConversationStream()
        mutableState.update { current ->
            val sameConversation = current.selectedAgent?.id == agent.id
            current.copy(
                selectedAgent = agent,
                conversation = current.conversation.takeIf { sameConversation },
                olderTurns = current.olderTurns.takeIf { sameConversation }.orEmpty(),
                historyBeforeCursor = current.historyBeforeCursor.takeIf { sameConversation },
                hasOlderTurns = sameConversation && current.hasOlderTurns,
                historyBusy = false,
                outbox = current.outbox.takeIf { sameConversation }.orEmpty(),
                draft = current.draft.takeIf { sameConversation }.orEmpty(),
                attachments = current.attachments.takeIf { sameConversation }.orEmpty(),
                agentSection = current.agentSection.takeIf { sameConversation } ?: AgentSection.Results,
                briefs = current.briefs.takeIf { sameConversation }.orEmpty(),
                selectedBrief = current.selectedBrief.takeIf { sameConversation },
                selectedTurn = current.selectedTurn.takeIf { sameConversation },
                conversationDetail = current.conversationDetail.takeIf { sameConversation },
                olderActivitiesBusy = false,
                olderActivitiesLoaded = sameConversation && current.olderActivitiesLoaded,
                selectedActivity = current.selectedActivity.takeIf { sameConversation },
                selectedToolExecution = current.selectedToolExecution.takeIf { sameConversation },
                workItems = current.workItems.takeIf { sameConversation }.orEmpty(),
                workItemsLimit = current.workItemsLimit.takeIf { sameConversation } ?: 30,
                workItemsHasMore = sameConversation && current.workItemsHasMore,
                workItemsLoadingMore = false,
                selectedWorkItem = current.selectedWorkItem.takeIf { sameConversation },
                workspaces = current.workspaces.takeIf { sameConversation }.orEmpty(),
                selectedWorkspace = current.selectedWorkspace.takeIf { sameConversation },
                workspaceDirectory = current.workspaceDirectory.takeIf { sameConversation },
                workspaceBusy = !sameConversation || current.workspaceBusy,
                preparedArtifact = current.preparedArtifact.takeIf { sameConversation },
                busy = true,
                error = null,
            )
        }
        conversationJob =
            viewModelScope.launch {
                runCatching {
                    withContext(Dispatchers.IO) { repository.conversation(agent) }
                }.onSuccess { bundle ->
                    mutableState.update {
                        val keepHistory = it.conversation?.eventLogEpoch == bundle.snapshot.eventLogEpoch &&
                            it.conversation?.runtimeId == bundle.snapshot.runtimeId
                        it.copy(
                            conversation = bundle.snapshot,
                            olderTurns = it.olderTurns.takeIf { keepHistory }.orEmpty(),
                            historyBeforeCursor = if (keepHistory && it.olderTurns.isNotEmpty()) it.historyBeforeCursor else bundle.snapshot.nextBeforeCursor,
                            hasOlderTurns = if (keepHistory && it.olderTurns.isNotEmpty()) it.hasOlderTurns else bundle.snapshot.hasMore,
                            outbox = bundle.outbox,
                            draft = if (sameAgentBeforeLoad) it.draft else bundle.draft,
                            attachments = if (sameAgentBeforeLoad) it.attachments else bundle.attachments,
                            busy = false,
                            online = true,
                        )
                    }
                    hydrateBriefs(agent, bundle.snapshot)
                    loadAgentWorkspace(agent)
                    startConversationStream(agent, bundle.snapshot.snapshotCursor)
                }.onFailure { error ->
                    if ((error is HolonHttpException && error.statusCode in setOf(401, 403)) ||
                        error is SessionScopeChangedException
                    ) {
                        handleRuntimeFailure(error)
                        return@onFailure
                    }
                    val cached = runCatching {
                        withContext(Dispatchers.IO) { repository.cachedConversation(agent.id) }
                    }.getOrNull()
                    if (cached != null) {
                        mutableState.update {
                            it.copy(
                                conversation = cached.snapshot,
                                outbox = cached.outbox,
                                draft = cached.draft,
                                attachments = cached.attachments,
                                busy = false,
                                online = false,
                                statusMessage = "会话暂时离线，显示缓存",
                            )
                        }
                        if (foreground) startConversationStream(agent, cached.snapshot.snapshotCursor)
                    } else {
                        handleRuntimeFailure(error)
                        if (foreground && state.value.phase == AppPhase.Ready) startConversationStream(agent, null)
                    }
                }
            }
    }

    fun closeConversation() {
        if (state.value.enqueueing) return
        conversationJob?.cancel()
        detailRefreshJob?.cancel()
        stopConversationStream()
        mutableState.update {
            it.copy(
                selectedAgent = null,
                conversation = null,
                olderTurns = emptyList(),
                historyBeforeCursor = null,
                hasOlderTurns = false,
                historyBusy = false,
                outbox = emptyList(),
                draft = "",
                attachments = emptyList(),
                agentSection = AgentSection.Results,
                briefs = emptyMap(),
                selectedBrief = null,
                selectedTurn = null,
                conversationDetail = null,
                olderActivitiesBusy = false,
                olderActivitiesLoaded = false,
                selectedActivity = null,
                selectedToolExecution = null,
                preparedArtifact = null,
                fileLinkOrigin = null,
                workItems = emptyList(),
                workItemsLimit = 30,
                workItemsHasMore = false,
                workItemsLoadingMore = false,
                selectedWorkItem = null,
                planFile = null,
                workspaces = emptyList(),
                selectedWorkspace = null,
                workspaceDirectory = null,
                detailBusy = false,
                workItemsBusy = false,
                workspaceBusy = false,
                error = null,
            )
        }
    }

    fun updateDraft(value: String) {
        val agent = state.value.selectedAgent ?: return
        draftRevision++
        mutableState.update { it.copy(draft = value) }
        draftSaveJobs.remove(agent.id)?.cancel()
        draftSaveJobs[agent.id] =
            viewModelScope.launch(Dispatchers.IO) { repository.saveDraft(agent.id, value) }
    }

    fun addAttachment(uri: Uri, preferredKind: String? = null) {
        val before = state.value
        val agent = before.selectedAgent ?: return
        if (before.stagingAttachment || before.enqueueing) return
        mutableState.update { it.copy(stagingAttachment = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    val attachment = repository.stageAttachment(
                        uri = uri,
                        preferredKind = preferredKind,
                        existingAttachments = before.attachments,
                        promptText = before.draft,
                    )
                    try {
                        repository.saveComposerAttachments(agent.id, before.attachments + attachment)
                    } catch (error: Throwable) {
                        repository.discardAttachment(attachment)
                        throw error
                    }
                    attachment
                }
            }.onSuccess { attachment ->
                mutableState.update {
                    it.copy(
                        attachments = if (it.selectedAgent?.id == agent.id) it.attachments + attachment else it.attachments,
                        stagingAttachment = false,
                        error = null,
                    )
                }
            }.onFailure { error -> mutableState.update { it.copy(stagingAttachment = false, error = humanError(error)) } }
        }
    }

    fun removeAttachment(index: Int) {
        if (state.value.stagingAttachment || state.value.enqueueing) return
        val agentId = state.value.selectedAgent?.id ?: return
        val removed = state.value.attachments.getOrNull(index)
        mutableState.update { current ->
            current.copy(attachments = current.attachments.filterIndexed { i, _ -> i != index })
        }
        removed?.let {
            val remaining = state.value.attachments
            composerSaveJobs[agentId] = viewModelScope.launch(Dispatchers.IO) {
                runCatching {
                    repository.saveComposerAttachments(agentId, remaining)
                    repository.discardAttachment(it)
                }.onFailure { error -> mutableState.update { state -> state.copy(error = humanError(error)) } }
            }
        }
    }

    fun send() {
        val current = state.value
        val agent = current.selectedAgent ?: return
        if (current.enqueueing || current.stagingAttachment) return
        if (current.draft.isBlank() && current.attachments.isEmpty()) return
        val text = current.draft
        val attachments = current.attachments
        val requestId = UUID.randomUUID().toString()
        val pendingDraftSave = draftSaveJobs[agent.id]
        val pendingComposerSave = composerSaveJobs[agent.id]
        val sentDraftRevision = draftRevision
        mutableState.update { it.copy(enqueueing = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    pendingDraftSave?.join()
                    pendingComposerSave?.join()
                    repository.enqueue(agent.id, text, attachments, requestId)
                }
            }.onSuccess { pending ->
                if (draftSaveJobs[agent.id] === pendingDraftSave) {
                    draftSaveJobs.remove(agent.id)
                }
                if (composerSaveJobs[agent.id] === pendingComposerSave) {
                    composerSaveJobs.remove(agent.id)
                }
                mutableState.update { value ->
                    value.copy(
                        draft =
                            if (
                                value.selectedAgent?.id == agent.id &&
                                draftRevision == sentDraftRevision
                            ) {
                                ""
                            } else {
                                value.draft
                            },
                        attachments = removeSentAttachments(value.attachments, attachments),
                        outbox = value.outbox + pending,
                        enqueueing = false,
                        error = null,
                    )
                }
                runCatching {
                    withContext(Dispatchers.IO) { repository.deliverOutbox(pending) }
                }.onSuccess { sent ->
                    mutableState.update { value ->
                        value.copy(
                            outbox = value.outbox.map {
                                if (it.requestId == sent.requestId) sent else it
                            },
                        )
                    }
                    openAgent(agent)
                    delay(250)
                    refresh(showProgress = false)
                }.onFailure(::handleRuntimeFailure)
            }.onFailure { error ->
                mutableState.update { it.copy(enqueueing = false, error = humanError(error)) }
            }
        }
    }

    fun retryMessage(message: OutboxEntity) {
        val current = state.value
        val agent = current.selectedAgent ?: return
        if (current.enqueueing || message.agentId != agent.id ||
            message.state !in setOf("failed", "unknown") ||
            current.outbox.none { it.requestId == message.requestId }
        ) return
        mutableState.update {
            it.copy(
                enqueueing = true,
                outbox = it.outbox.map { pending ->
                    if (pending.requestId == message.requestId) pending.copy(state = "sending", error = null) else pending
                },
            )
        }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.deliverOutbox(message) }
            }.onSuccess { result ->
                mutableState.update { value ->
                    value.copy(
                        enqueueing = false,
                        outbox = value.outbox.map { if (it.requestId == result.requestId) result else it },
                    )
                }
                if (result.state == "received") refresh(showProgress = false)
            }.onFailure { error ->
                mutableState.update { value ->
                    value.copy(
                        enqueueing = false,
                        outbox = value.outbox.map { if (it.requestId == message.requestId) message else it },
                    )
                }
                handleRuntimeFailure(error)
            }
        }
    }

    fun editFailedMessage(message: OutboxEntity) {
        val current = state.value
        if (current.enqueueing || current.selectedAgent?.id != message.agentId || message.state != "failed") return
        if (current.draft.isNotBlank() || current.attachments.isNotEmpty()) {
            mutableState.update { it.copy(error = "输入框已有内容，请先发送或清空，再编辑失败消息") }
            return
        }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.editFailedOutbox(message) }
            }.onSuccess { attachments ->
                draftRevision++
                mutableState.update {
                    it.copy(
                        draft = message.text,
                        attachments = attachments,
                        outbox = it.outbox.filterNot { pending -> pending.requestId == message.requestId },
                        error = null,
                        statusMessage = "已移回输入框；修改后发送会使用新的请求 ID",
                    )
                }
            }.onFailure { error -> mutableState.update { it.copy(error = humanError(error)) } }
        }
    }

    fun removeFailedMessage(message: OutboxEntity) {
        val current = state.value
        if (current.enqueueing || current.selectedAgent?.id != message.agentId || message.state != "failed") return
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.removeFailedOutbox(message) }
            }.onSuccess {
                mutableState.update {
                    it.copy(
                        outbox = it.outbox.filterNot { pending -> pending.requestId == message.requestId },
                        statusMessage = "已从本机移除未发送消息",
                    )
                }
            }.onFailure { error -> mutableState.update { it.copy(error = humanError(error)) } }
        }
    }

    private fun removeSentAttachments(
        current: List<StagedAttachment>,
        sent: List<StagedAttachment>,
    ): List<StagedAttachment> {
        val sentPaths = sent.mapTo(mutableSetOf(), StagedAttachment::localPath)
        return current.filterNot { it.localPath in sentPaths }
    }

    fun openBrief(briefId: String) {
        val agent = state.value.selectedAgent ?: return
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.brief(agent.id, briefId) }
            }.onSuccess { brief ->
                if (state.value.selectedAgent?.id == agent.id) {
                    mutableState.update {
                        it.copy(
                            selectedBrief = brief,
                            briefs = it.briefs + (brief.id to brief),
                            busy = false,
                        )
                    }
                }
            }.onFailure { error ->
                if (state.value.selectedAgent?.id == agent.id) handleRuntimeFailure(error)
            }
        }
    }

    fun closeBrief() = mutableState.update {
        it.copy(selectedBrief = null, preparedArtifact = null, agentSection = AgentSection.Results)
    }

    fun selectAgentSection(section: AgentSection) {
        mutableState.update {
            it.copy(
                agentSection = section,
                selectedTurn = null,
                conversationDetail = null,
                olderActivitiesBusy = false,
                olderActivitiesLoaded = false,
                selectedActivity = null,
                selectedToolExecution = null,
                selectedWorkItem = null,
                planFile = null,
                preparedArtifact = null,
                fileLinkOrigin = null,
            )
        }
    }

    fun openTurn(turn: HolonConversationTurn) {
        val agent = state.value.selectedAgent ?: return
        mutableState.update {
            it.copy(
                selectedTurn = turn,
                conversationDetail = null,
                olderActivitiesBusy = false,
                olderActivitiesLoaded = false,
                selectedActivity = null,
                selectedToolExecution = null,
                detailBusy = true,
                error = null,
            )
        }
        scheduleTurnDetailRefresh(agent, turn.id, delayMillis = 0)
    }

    fun loadOlderTurns() {
        val current = state.value
        val agent = current.selectedAgent ?: return
        val before = current.historyBeforeCursor ?: return
        if (!current.hasOlderTurns || current.historyBusy) return
        val epoch = current.conversation?.eventLogEpoch
        mutableState.update { it.copy(historyBusy = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.olderConversation(agent.id, before) }
            }.onSuccess { page ->
                if (state.value.selectedAgent?.id == agent.id && state.value.conversation?.eventLogEpoch == epoch) {
                    if (page.eventLogEpoch != epoch) {
                        mutableState.update { it.copy(historyBusy = false, error = "会话记录已重置，请刷新后重试") }
                        return@onSuccess
                    }
                    mutableState.update {
                        val newerIds = it.conversation?.turns.orEmpty().mapTo(mutableSetOf(), HolonConversationTurn::id)
                        val older = (page.turns + it.olderTurns).distinctBy(HolonConversationTurn::id)
                            .filterNot { turn -> turn.id in newerIds }
                        it.copy(
                            olderTurns = older,
                            historyBeforeCursor = page.nextBeforeCursor,
                            hasOlderTurns = page.hasMore,
                            historyBusy = false,
                        )
                    }
                    hydrateBriefs(agent, page)
                }
            }.onFailure { error ->
                mutableState.update { it.copy(historyBusy = false) }
                handleRuntimeFailure(error)
            }
        }
    }

    fun loadOlderActivities() {
        val current = state.value
        val agent = current.selectedAgent ?: return
        val turnId = current.selectedTurn?.id ?: return
        val detail = current.conversationDetail ?: return
        val before = detail.nextBeforeCursor ?: return
        if (!detail.hasMore || current.olderActivitiesBusy) return
        mutableState.update { it.copy(olderActivitiesBusy = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.conversationDetail(agent.id, turnId, before) }
            }.onSuccess { page ->
                if (state.value.selectedAgent?.id == agent.id && state.value.selectedTurn?.id == turnId) {
                    if (state.value.conversationDetail?.eventLogEpoch != page.eventLogEpoch) {
                        mutableState.update { it.copy(olderActivitiesBusy = false, error = "执行记录已重置，请重新打开本轮过程") }
                        return@onSuccess
                    }
                    mutableState.update {
                        val latest = it.conversationDetail ?: return@update it.copy(olderActivitiesBusy = false)
                        val latestIds = latest.activities.mapTo(mutableSetOf(), HolonConversationActivity::id)
                        val activities = page.activities.filterNot { activity -> activity.id in latestIds } + latest.activities
                        it.copy(
                            conversationDetail = latest.copy(
                                activities = activities,
                                coverageKind = if (page.coverageKind != "complete") page.coverageKind else latest.coverageKind,
                                coverageReason = page.coverageReason ?: latest.coverageReason,
                                hasMore = page.hasMore,
                                nextBeforeCursor = page.nextBeforeCursor,
                            ),
                            olderActivitiesBusy = false,
                            olderActivitiesLoaded = true,
                        )
                    }
                }
            }.onFailure { error ->
                mutableState.update { it.copy(olderActivitiesBusy = false) }
                handleRuntimeFailure(error)
            }
        }
    }

    fun closeTurn() {
        detailRefreshJob?.cancel()
        mutableState.update {
            it.copy(
                selectedTurn = null,
                conversationDetail = null,
                olderActivitiesBusy = false,
                olderActivitiesLoaded = false,
                selectedActivity = null,
                selectedToolExecution = null,
                detailBusy = false,
            )
        }
    }

    fun inspectActivity(activity: HolonConversationActivity) {
        val agent = state.value.selectedAgent ?: return
        val toolId = activity.toolExecutionId
        mutableState.update {
            it.copy(
                selectedActivity = activity,
                selectedToolExecution = null,
                detailBusy = toolId != null,
                error = null,
            )
        }
        if (toolId == null) return
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.toolExecution(agent.id, toolId) }
            }.onSuccess { detail ->
                if (state.value.selectedActivity?.id == activity.id) {
                    mutableState.update { it.copy(selectedToolExecution = detail, detailBusy = false) }
                }
            }.onFailure { error ->
                mutableState.update { it.copy(detailBusy = false, error = humanError(error)) }
            }
        }
    }

    fun closeActivity() =
        mutableState.update { it.copy(selectedActivity = null, selectedToolExecution = null, detailBusy = false) }

    fun openWorkItem(item: HolonWorkItemSnapshot) {
        val agent = state.value.selectedAgent ?: return
        mutableState.update { it.copy(selectedWorkItem = item, planFile = null, workItemsBusy = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.workItem(agent.id, item.workItemId) }
            }.onSuccess { detail ->
                if (state.value.selectedWorkItem?.workItemId == item.workItemId) {
                    mutableState.update { it.copy(selectedWorkItem = detail, workItemsBusy = false) }
                }
            }.onFailure { error ->
                mutableState.update { it.copy(workItemsBusy = false, error = humanError(error)) }
            }
        }
    }

    fun openRelatedWorkItem(workItemId: String) {
        val agent = state.value.selectedAgent ?: return
        if (state.value.busy) return
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.workItem(agent.id, workItemId) }
            }.onSuccess { detail ->
                if (state.value.selectedAgent?.id == agent.id) {
                    mutableState.update {
                        it.copy(
                            agentSection = AgentSection.Work,
                            selectedWorkItem = detail,
                            planFile = null,
                            busy = false,
                        )
                    }
                }
            }.onFailure { error ->
                if (state.value.selectedAgent?.id == agent.id) {
                    mutableState.update { it.copy(busy = false, error = humanError(error)) }
                }
            }
        }
    }

    fun openWorkItemPlan() {
        val agent = state.value.selectedAgent ?: return
        val item = state.value.selectedWorkItem ?: return
        val plan = item.planArtifact ?: return
        if (state.value.workItemsBusy) return
        mutableState.update { it.copy(workItemsBusy = true, planFile = null, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.prepareWorkItemPlan(agent.id, plan) }
            }.onSuccess { file ->
                if (state.value.selectedAgent?.id == agent.id && state.value.selectedWorkItem?.workItemId == item.workItemId) {
                    mutableState.update { it.copy(planFile = file, workItemsBusy = false) }
                }
            }.onFailure { error ->
                mutableState.update { it.copy(workItemsBusy = false) }
                if ((error is HolonHttpException && error.statusCode in setOf(401, 403)) ||
                    error is SessionScopeChangedException
                ) {
                    handleRuntimeFailure(error)
                } else {
                    mutableState.update { it.copy(error = "无法打开计划：${humanError(error)}") }
                }
            }
        }
    }

    fun closePlanFile() = mutableState.update { it.copy(planFile = null) }

    fun closeWorkItem() = mutableState.update { it.copy(selectedWorkItem = null, planFile = null) }

    fun loadMoreWorkItems() {
        val current = state.value
        val agent = current.selectedAgent ?: return
        if (!current.workItemsHasMore || current.workItemsLoadingMore) return
        val nextLimit = current.workItemsLimit + 50
        mutableState.update { it.copy(workItemsLoadingMore = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.workItems(agent.id, nextLimit) }
            }.onSuccess { items ->
                if (state.value.selectedAgent?.id == agent.id) {
                    mutableState.update {
                        it.copy(
                            workItems = items,
                            workItemsLimit = nextLimit,
                            workItemsHasMore = items.size > it.workItems.size && items.size >= nextLimit,
                            workItemsLoadingMore = false,
                        )
                    }
                }
            }.onFailure { error ->
                mutableState.update { it.copy(workItemsLoadingMore = false, error = humanError(error)) }
            }
        }
    }

    fun selectWorkspace(workspace: HolonWorkspace) {
        mutableState.update {
            it.copy(
                selectedWorkspace = workspace,
                workspaceDirectory = null,
                preparedArtifact = null,
                workspaceBusy = true,
                error = null,
            )
        }
        browseWorkspace(workspace, "")
    }

    fun openMessageFile(reference: MessageFileReference) {
        val agentId = state.value.selectedAgent?.id ?: return
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.resolveFileReference(reference.reference) }
            }.onSuccess { result ->
                if (state.value.selectedAgent?.id != agentId) return@onSuccess
                when (result) {
                    is HolonFileReferenceResult.Unresolved -> mutableState.update {
                        it.copy(error = "文件无法打开：${result.message}")
                    }
                    is HolonFileReferenceResult.Resolved -> {
                        val location = result.location
                        if (location.kind !in setOf("file", "directory")) {
                            mutableState.update { it.copy(error = "不支持的文件类型：${location.kind}") }
                            return@onSuccess
                        }
                        val workspace = state.value.workspaces.firstOrNull {
                            it.workspaceId == location.workspaceId && it.executionRootId == location.executionRootId
                        } ?: HolonWorkspace(
                            workspaceId = location.workspaceId,
                            alias = null,
                            label = location.workspaceId,
                            isActive = false,
                            executionRootId = location.executionRootId,
                            projectionKind = location.rootKind,
                        )
                        val directory = if (location.kind == "directory") location.path else location.path.substringBeforeLast('/', "")
                        mutableState.update {
                            it.copy(
                                fileLinkOrigin = it.fileLinkOrigin ?: FileLinkOrigin(
                                    section = it.agentSection,
                                    brief = it.selectedBrief,
                                    turn = it.selectedTurn,
                                    activity = it.selectedActivity,
                                    workItem = it.selectedWorkItem,
                                    planFile = it.planFile,
                                    workspace = it.selectedWorkspace,
                                    directory = it.workspaceDirectory,
                                ),
                                agentSection = AgentSection.Files,
                                selectedBrief = null,
                                selectedTurn = null,
                                selectedActivity = null,
                                selectedWorkItem = null,
                                planFile = null,
                                preparedArtifact = null,
                                selectedWorkspace = workspace,
                                workspaces = if (workspace in it.workspaces) it.workspaces else it.workspaces + workspace,
                                workspaceDirectory = null,
                                workspaceBusy = true,
                                error = null,
                                statusMessage = if (reference.fragment != null) "已打开文件；段落定位暂不可用" else null,
                            )
                        }
                        browseWorkspace(workspace, directory)
                        if (location.kind == "file") {
                            viewModelScope.launch {
                                runCatching {
                                    withContext(Dispatchers.IO) { repository.prepareWorkspaceFile(workspace, location.path) }
                                }.onSuccess { artifact ->
                                    if (state.value.selectedAgent?.id == agentId && state.value.fileLinkOrigin != null &&
                                        state.value.agentSection == AgentSection.Files && state.value.selectedWorkspace == workspace
                                    ) {
                                        mutableState.update { it.copy(preparedArtifact = artifact, workspaceBusy = false) }
                                    }
                                }.onFailure { error ->
                                    if (state.value.fileLinkOrigin != null) {
                                        if (error is HolonHttpException && error.statusCode in setOf(401, 403)) handleRuntimeFailure(error)
                                        else mutableState.update { it.copy(workspaceBusy = false, error = humanError(error)) }
                                    }
                                }
                            }
                        }
                    }
                }
            }.onFailure { error ->
                if (state.value.selectedAgent?.id != agentId) return@onFailure
                if (error is HolonHttpException && error.statusCode in setOf(401, 403)) handleRuntimeFailure(error)
                else mutableState.update { it.copy(error = humanError(error)) }
            }
        }
    }

    fun openWorkspaceEntry(name: String, directory: Boolean) {
        val workspace = state.value.selectedWorkspace ?: return
        val currentPath = state.value.workspaceDirectory?.path.orEmpty().trim('/')
        val path = listOf(currentPath, name).filter(String::isNotBlank).joinToString("/")
        if (directory) {
            mutableState.update { it.copy(workspaceBusy = true, preparedArtifact = null, error = null) }
            browseWorkspace(workspace, path)
        } else {
            mutableState.update { it.copy(workspaceBusy = true, preparedArtifact = null, error = null) }
            viewModelScope.launch {
                runCatching {
                    withContext(Dispatchers.IO) { repository.prepareWorkspaceFile(workspace, path) }
                }.onSuccess { artifact ->
                    mutableState.update { it.copy(preparedArtifact = artifact, workspaceBusy = false) }
                }.onFailure { error ->
                    mutableState.update { it.copy(workspaceBusy = false, error = humanError(error)) }
                }
            }
        }
    }

    fun navigateWorkspaceUp() {
        val workspace = state.value.selectedWorkspace ?: return
        val currentPath = state.value.workspaceDirectory?.path.orEmpty().trim('/')
        if (currentPath.isEmpty()) return
        val parent = currentPath.substringBeforeLast('/', "")
        navigateWorkspaceTo(parent)
    }

    fun navigateWorkspaceTo(path: String) {
        val workspace = state.value.selectedWorkspace ?: return
        mutableState.update { it.copy(workspaceBusy = true, preparedArtifact = null, error = null) }
        browseWorkspace(workspace, path.trim('/'))
    }

    fun prepareArtifact(locator: String, name: String) {
        mutableState.update { it.copy(busy = true, error = null, preparedArtifact = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.prepareArtifact(locator, name) }
            }.onSuccess { artifact ->
                mutableState.update { it.copy(preparedArtifact = artifact, busy = false) }
            }.onFailure(::handleRuntimeFailure)
        }
    }

    fun stopCurrentTurn() {
        val agent = state.value.selectedAgent ?: return
        val runId = agent.currentRunId ?: return
        if (state.value.abortingRun) return
        mutableState.update { it.copy(abortingRun = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.abortCurrentRun(agent.id, runId) }
            }.onSuccess {
                mutableState.update { it.copy(abortingRun = false, statusMessage = "正在停止本轮…") }
                delay(250)
                refresh(showProgress = false)
            }.onFailure { error ->
                mutableState.update { it.copy(abortingRun = false, error = humanError(error)) }
            }
        }
    }

    fun clearPreparedArtifact() = mutableState.update { it.copy(preparedArtifact = null) }

    fun returnFromMessageFile() {
        val origin = state.value.fileLinkOrigin
        if (origin == null) {
            clearPreparedArtifact()
            return
        }
        workspaceBrowseJob?.cancel()
        mutableState.update {
            it.copy(
                agentSection = origin.section,
                selectedBrief = origin.brief,
                selectedTurn = origin.turn,
                selectedActivity = origin.activity,
                selectedWorkItem = origin.workItem,
                planFile = origin.planFile,
                selectedWorkspace = origin.workspace,
                workspaceDirectory = origin.directory,
                workspaceBusy = false,
                preparedArtifact = null,
                fileLinkOrigin = null,
                statusMessage = null,
            )
        }
    }

    fun saveArtifactToDevice(artifact: PreparedArtifact, destination: Uri) {
        mutableState.update { it.copy(statusMessage = "正在保存 ${artifact.fileName}…", error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.saveArtifactToDevice(artifact, destination) }
            }.onSuccess {
                mutableState.update { it.copy(statusMessage = "已保存到设备：${artifact.fileName}") }
            }.onFailure { error ->
                mutableState.update {
                    it.copy(
                        statusMessage = null,
                        error = "保存失败：${humanError(error)}。所选位置可能留下不完整文件。",
                    )
                }
            }
        }
    }

    fun selectMainDestination(destination: MainDestination) {
        mutableState.update { it.copy(mainDestination = destination) }
    }

    fun handleSystemBack(): Boolean {
        val current = state.value
        return when {
            current.fileLinkOrigin != null -> {
                returnFromMessageFile()
                true
            }
            current.planFile != null -> {
                closePlanFile()
                true
            }
            current.preparedArtifact != null -> {
                clearPreparedArtifact()
                true
            }
            current.selectedActivity != null -> {
                closeActivity()
                true
            }
            current.selectedTurn != null -> {
                closeTurn()
                true
            }
            current.selectedWorkItem != null -> {
                closeWorkItem()
                true
            }
            current.selectedBrief != null -> {
                closeBrief()
                true
            }
            current.selectedAgent != null &&
                current.agentSection == AgentSection.Files &&
                !current.workspaceDirectory?.path.isNullOrBlank() -> {
                navigateWorkspaceUp()
                true
            }
            current.selectedAgent != null -> {
                closeConversation()
                true
            }
            current.mainDestination != MainDestination.Agents -> {
                selectMainDestination(MainDestination.Agents)
                true
            }
            else -> false
        }
    }

    fun relogin() = endSession(keepCurrentHost = true)

    fun logout() = endSession(keepCurrentHost = false)

    private fun endSession(keepCurrentHost: Boolean) {
        if (state.value.enqueueing) return
        stopConversationStream()
        val nextBaseUrl = if (keepCurrentHost) state.value.baseUrl else defaultBaseUrl()
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            withContext(Dispatchers.IO) { repository.logout() }
            mutableState.value = HolonUiState(phase = AppPhase.SignedOut, baseUrl = nextBaseUrl)
        }
    }

    fun clearError() = mutableState.update { it.copy(error = null, statusMessage = null) }

    private fun startConversationStream(agent: AgentSummary, after: String?) {
        stopConversationStream()
        conversationStreamJob =
            viewModelScope.launch(Dispatchers.IO) {
                var cursor = after
                var retryDelay = 1_000L
                while (isActive && foreground && state.value.selectedAgent?.id == agent.id) {
                    try {
                        if (!state.value.online) {
                            val (session, roster) = repository.refreshSessionAndRoster()
                            withContext(Dispatchers.Main) {
                                if (state.value.selectedAgent?.id == agent.id) {
                                    mutableState.update { it.copy(session = session, agents = roster.agents) }
                                }
                            }
                        }
                        var reopenAfterReset = false
                        val connection = repository.openConversationStream(agent.id, cursor)
                        conversationStream = connection
                        try {
                            for (event in connection.events()) {
                                val change = event.toConversationEvent()
                                if (change is HolonConversationStreamEvent.Mutation &&
                                    change.type in setOf("activity_upsert", "detail_invalidated", "turn_summary_upsert")
                                ) {
                                    state.value.selectedTurn?.id?.let { turnId ->
                                        scheduleTurnDetailRefresh(agent, turnId)
                                    }
                                }
                                if (change is HolonConversationStreamEvent.Checkpoint ||
                                    change is HolonConversationStreamEvent.ResetRequired
                                ) {
                                    val bundle = repository.conversation(agent)
                                    cursor = bundle.snapshot.snapshotCursor
                                    withContext(Dispatchers.Main) {
                                        if (state.value.selectedAgent?.id == agent.id) {
                                            mutableState.update {
                                                val selectedTurnId = it.selectedTurn?.id
                                                val keepHistory = it.conversation?.eventLogEpoch == bundle.snapshot.eventLogEpoch &&
                                                    it.conversation?.runtimeId == bundle.snapshot.runtimeId
                                                it.copy(
                                                    conversation = bundle.snapshot,
                                                    olderTurns = it.olderTurns.takeIf { keepHistory }.orEmpty(),
                                                    historyBeforeCursor = if (keepHistory && it.olderTurns.isNotEmpty()) it.historyBeforeCursor else bundle.snapshot.nextBeforeCursor,
                                                    hasOlderTurns = if (keepHistory && it.olderTurns.isNotEmpty()) it.hasOlderTurns else bundle.snapshot.hasMore,
                                                    selectedTurn =
                                                        selectedTurnId?.let { id ->
                                                            bundle.snapshot.turns.firstOrNull { turn -> turn.id == id }
                                                        } ?: it.selectedTurn,
                                                    outbox = bundle.outbox,
                                                    online = true,
                                                    lastSyncedAt = System.currentTimeMillis(),
                                                    statusMessage = null,
                                                )
                                            }
                                        }
                                    }
                                    state.value.selectedTurn?.id?.let { turnId ->
                                        scheduleTurnDetailRefresh(agent, turnId)
                                    }
                                    retryDelay = 1_000L
                                    if (change is HolonConversationStreamEvent.ResetRequired) {
                                        reopenAfterReset = true
                                        break
                                    }
                                    hydrateBriefs(agent, bundle.snapshot)
                                }
                            }
                        } finally {
                            connection.close()
                            if (conversationStream === connection) conversationStream = null
                        }
                        if (reopenAfterReset) continue
                        withContext(Dispatchers.Main) { markConnectionInterrupted(agent.id) }
                    } catch (_: CancellationException) {
                        break
                    } catch (error: Throwable) {
                        if ((error is HolonHttpException && error.statusCode in setOf(401, 403)) ||
                            error is SessionScopeChangedException
                        ) {
                            withContext(Dispatchers.Main) { handleRuntimeFailure(error) }
                            break
                        }
                        withContext(Dispatchers.Main) { markConnectionInterrupted(agent.id) }
                    }
                    delay(retryDelay)
                    retryDelay = (retryDelay * 2).coerceAtMost(30_000L)
                    if (!isActive || !foreground || state.value.selectedAgent?.id != agent.id) break
                    try {
                        val (session, roster) = repository.refreshSessionAndRoster()
                        val bundle = repository.conversation(agent)
                        cursor = bundle.snapshot.snapshotCursor
                        withContext(Dispatchers.Main) {
                            if (state.value.selectedAgent?.id == agent.id) {
                                mutableState.update {
                                    val keepHistory = it.conversation?.eventLogEpoch == bundle.snapshot.eventLogEpoch &&
                                        it.conversation?.runtimeId == bundle.snapshot.runtimeId
                                    it.copy(
                                        session = session,
                                        agents = roster.agents,
                                        conversation = bundle.snapshot,
                                        olderTurns = it.olderTurns.takeIf { keepHistory }.orEmpty(),
                                        historyBeforeCursor = if (keepHistory && it.olderTurns.isNotEmpty()) it.historyBeforeCursor else bundle.snapshot.nextBeforeCursor,
                                        hasOlderTurns = if (keepHistory && it.olderTurns.isNotEmpty()) it.hasOlderTurns else bundle.snapshot.hasMore,
                                        outbox = bundle.outbox,
                                        online = true,
                                        lastSyncedAt = System.currentTimeMillis(),
                                        statusMessage = null,
                                    )
                                }
                                hydrateBriefs(agent, bundle.snapshot)
                            }
                        }
                    } catch (_: CancellationException) {
                        break
                    } catch (error: Throwable) {
                        if ((error is HolonHttpException && error.statusCode in setOf(401, 403)) ||
                            error is SessionScopeChangedException
                        ) {
                            withContext(Dispatchers.Main) { handleRuntimeFailure(error) }
                            break
                        }
                    }
                }
            }
    }

    private fun markConnectionInterrupted(agentId: String) {
        if (state.value.selectedAgent?.id != agentId) return
        mutableState.update {
            it.copy(online = false, statusMessage = "连接中断，正在重连；当前显示上次同步的内容")
        }
    }

    private fun scheduleTurnDetailRefresh(
        agent: AgentSummary,
        turnId: String,
        delayMillis: Long = 180,
    ) {
        detailRefreshJob?.cancel()
        detailRefreshJob =
            viewModelScope.launch {
                if (delayMillis > 0) delay(delayMillis)
                runCatching {
                    withContext(Dispatchers.IO) { repository.conversationDetail(agent.id, turnId) }
                }.onSuccess { detail ->
                    if (state.value.selectedAgent?.id == agent.id && state.value.selectedTurn?.id == turnId) {
                        mutableState.update {
                            val previous = it.conversationDetail
                            val keepOlder = it.olderActivitiesLoaded && previous?.eventLogEpoch == detail.eventLogEpoch
                            val merged = if (keepOlder && previous != null) {
                                val latestIds = detail.activities.mapTo(mutableSetOf(), HolonConversationActivity::id)
                                detail.copy(
                                    activities = previous.activities.filterNot { activity -> activity.id in latestIds } + detail.activities,
                                    hasMore = previous.hasMore,
                                    nextBeforeCursor = previous.nextBeforeCursor,
                                )
                            } else detail
                            it.copy(
                                conversationDetail = merged,
                                olderActivitiesLoaded = keepOlder,
                                detailBusy = false,
                            )
                        }
                    }
                }.onFailure { error ->
                    if (state.value.selectedTurn?.id == turnId) {
                        mutableState.update { it.copy(detailBusy = false, error = humanError(error)) }
                    }
                }
            }
    }

    private fun stopConversationStream() {
        conversationStream?.close()
        conversationStream = null
        conversationStreamJob?.cancel()
        conversationStreamJob = null
    }

    private fun hydrateBriefs(agent: AgentSummary, snapshot: HolonConversationSnapshot) {
        val briefIds = snapshot.turns.asReversed().flatMap(HolonConversationTurn::briefIds).distinct().take(20)
        val missing = briefIds.filterNot(state.value.briefs::containsKey)
        if (missing.isEmpty()) return
        viewModelScope.launch {
            missing.forEach { briefId ->
                runCatching {
                    withContext(Dispatchers.IO) { repository.brief(agent.id, briefId) }
                }.onSuccess { brief ->
                    if (state.value.selectedAgent?.id == agent.id) {
                        mutableState.update { it.copy(briefs = it.briefs + (brief.id to brief)) }
                    }
                }
            }
        }
    }

    private fun loadAgentWorkspace(agent: AgentSummary) {
        val activeWorkspace =
            agent.workspaceId?.let { workspaceId ->
                HolonWorkspace(
                    workspaceId = workspaceId,
                    alias = null,
                    label = agent.workspaceLabel?.trimEnd('/')?.substringAfterLast('/') ?: workspaceId,
                    isActive = true,
                    executionRootId = agent.executionRootId,
                    projectionKind = agent.workspaceProjectionKind,
                )
            }
        val previousWorkspace = state.value.selectedWorkspace
        val initialWorkspace = previousWorkspace ?: activeWorkspace
        val initialPath = state.value.workspaceDirectory?.path.orEmpty()
        mutableState.update {
            it.copy(
                workItemsBusy = true,
                workspaceBusy = true,
                workspaces = it.workspaces.ifEmpty { initialWorkspace?.let(::listOf).orEmpty() },
                selectedWorkspace = initialWorkspace,
            )
        }
        initialWorkspace?.let { browseWorkspace(it, initialPath) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.workItems(agent.id, state.value.workItemsLimit) }
            }.onSuccess { items ->
                if (state.value.selectedAgent?.id == agent.id) {
                    mutableState.update {
                        it.copy(
                            workItems = items,
                            workItemsHasMore = items.size >= it.workItemsLimit,
                            workItemsBusy = false,
                        )
                    }
                }
            }.onFailure { mutableState.update { it.copy(workItemsBusy = false) } }
        }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.workspaces(agent.id) }
            }.onSuccess { workspaces ->
                if (state.value.selectedAgent?.id == agent.id) {
                    val workspace = workspaces.firstOrNull {
                        it.workspaceId == previousWorkspace?.workspaceId && it.executionRootId == previousWorkspace?.executionRootId
                    } ?: workspaces.firstOrNull { it.isActive } ?: workspaces.firstOrNull()
                    val previous = state.value.selectedWorkspace
                    mutableState.update {
                        it.copy(
                            workspaces = workspaces,
                            selectedWorkspace = workspace,
                            workspaceBusy = workspace != null && (workspace != previous || it.workspaceDirectory == null),
                        )
                    }
                    if (workspace != null && workspace != previous) browseWorkspace(workspace, "")
                }
            }.onFailure {
                if (activeWorkspace == null) mutableState.update { it.copy(workspaceBusy = false) }
            }
        }
    }

    private fun browseWorkspace(workspace: HolonWorkspace, path: String) {
        workspaceBrowseJob?.cancel()
        val request =
            WorkspaceBrowseRequest(
                generation = ++workspaceBrowseGeneration,
                workspaceId = workspace.workspaceId,
                executionRootId = workspace.executionRootId,
                path = path,
            )
        workspaceBrowseRequest = request
        workspaceBrowseJob = viewModelScope.launch {
            executeWorkspaceBrowseRequest {
                withContext(Dispatchers.IO) { repository.browseWorkspace(workspace, path) }
            }.onSuccess { directory ->
                if (request.appliesTo(workspaceBrowseRequest, state.value.selectedWorkspace)) {
                    mutableState.update { it.copy(workspaceDirectory = directory, workspaceBusy = false) }
                }
            }.onFailure { error ->
                if (request.appliesTo(workspaceBrowseRequest, state.value.selectedWorkspace)) {
                    mutableState.update { it.copy(workspaceBusy = false, error = humanError(error)) }
                }
            }
        }
    }

    private fun handleRuntimeFailure(error: Throwable) {
        if ((error is HolonHttpException && error.statusCode in setOf(401, 403)) ||
            error is SessionScopeChangedException
        ) {
            viewModelScope.launch {
                withContext(Dispatchers.IO) { repository.logout() }
                mutableState.value =
                    HolonUiState(
                        phase = AppPhase.SignedOut,
                        baseUrl = state.value.baseUrl,
                        error = "登录已失效，请重新登录",
                    )
            }
            return
        }
        mutableState.update {
            it.copy(busy = false, online = false, error = humanError(error))
        }
    }

    companion object {
        fun factory(application: Application, container: AppContainer): ViewModelProvider.Factory =
            object : ViewModelProvider.Factory {
                @Suppress("UNCHECKED_CAST")
                override fun <T : ViewModel> create(modelClass: Class<T>): T =
                    HolonViewModel(application, container.repository) as T
            }
    }
}

internal data class WorkspaceBrowseRequest(
    val generation: Long,
    val workspaceId: String,
    val executionRootId: String?,
    val path: String,
)

internal fun WorkspaceBrowseRequest.appliesTo(
    currentRequest: WorkspaceBrowseRequest?,
    selectedWorkspace: HolonWorkspace?,
): Boolean =
    currentRequest == this &&
        selectedWorkspace?.workspaceId == workspaceId &&
        selectedWorkspace.executionRootId == executionRootId

internal suspend fun <T> executeWorkspaceBrowseRequest(block: suspend () -> T): Result<T> =
    try {
        Result.success(block())
    } catch (error: CancellationException) {
        throw error
    } catch (error: Throwable) {
        Result.failure(error)
    }

private fun ConversationCacheEntity.toAgentSummary(): AgentSummary =
    AgentSummary(
        id = agentId,
        displayName = displayName,
        isDefault = false,
        registryStatus = "cached",
        runtimeStatus = "offline",
        effectiveModel = "",
        pending = pending,
        currentRunId = null,
        schedulingPosture = posture,
        waitingReason = waitingReason,
        latestBrief = latestBriefId?.let {
            run.holon.android.sdk.HolonLatestBrief(it, latestActivityAt.orEmpty(), latestBriefPreview.orEmpty(), null)
        },
    )

internal fun AgentSummary.needsReply(): Boolean =
    schedulingPosture == "waiting_for_operator" || waitingReason == "awaiting_operator_input"

internal fun AgentSummary.hasUnreadBrief(readBriefIds: Map<String, String>): Boolean =
    latestBrief?.briefId?.let { it != readBriefIds[id] } ?: false
