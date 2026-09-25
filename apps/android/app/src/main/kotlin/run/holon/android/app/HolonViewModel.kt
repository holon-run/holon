package run.holon.android.app

import android.app.Application
import android.content.Context
import android.net.Uri
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewModelScope
import java.util.UUID
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
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonConversationDetail
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonConversationStreamEvent
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonHttpException
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

internal data class HolonUiState(
    val phase: AppPhase = AppPhase.Starting,
    val baseUrl: String = "",
    val token: String = "",
    val showToken: Boolean = false,
    val allowInsecureHttp: Boolean = false,
    val mainDestination: MainDestination = MainDestination.Agents,
    val busy: Boolean = false,
    val enqueueing: Boolean = false,
    val abortingRun: Boolean = false,
    val online: Boolean = false,
    val session: ActiveSession? = null,
    val agents: List<AgentSummary> = emptyList(),
    val selectedAgent: AgentSummary? = null,
    val conversation: HolonConversationSnapshot? = null,
    val outbox: List<OutboxEntity> = emptyList(),
    val draft: String = "",
    val attachments: List<StagedAttachment> = emptyList(),
    val agentSection: AgentSection = AgentSection.Results,
    val briefs: Map<String, HolonBrief> = emptyMap(),
    val selectedBrief: HolonBrief? = null,
    val selectedTurn: HolonConversationTurn? = null,
    val conversationDetail: HolonConversationDetail? = null,
    val selectedActivity: HolonConversationActivity? = null,
    val selectedToolExecution: HolonToolExecutionSnapshot? = null,
    val detailBusy: Boolean = false,
    val workItems: List<HolonWorkItemSnapshot> = emptyList(),
    val selectedWorkItem: HolonWorkItemSnapshot? = null,
    val workItemsBusy: Boolean = false,
    val workspaces: List<HolonWorkspace> = emptyList(),
    val selectedWorkspace: HolonWorkspace? = null,
    val workspaceDirectory: HolonWorkspaceDirectory? = null,
    val workspaceBusy: Boolean = false,
    val preparedArtifact: PreparedArtifact? = null,
    val error: String? = null,
    val statusMessage: String? = null,
    val search: String = "",
) {
    val recentAgents: List<AgentSummary>
        get() =
            agents.sortedWith(
                compareByDescending<AgentSummary> { it.needsReply() }
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
    private var detailRefreshJob: Job? = null
    private val draftSaveJobs = mutableMapOf<String, Job>()
    private var draftRevision = 0L

    init {
        viewModelScope.launch {
            val result = withContext(Dispatchers.IO) { repository.resume() }
            when (result) {
                ResumeResult.NoSession ->
                    mutableState.update { it.copy(phase = AppPhase.SignedOut) }
                is ResumeResult.Ready ->
                    mutableState.update {
                        it.copy(
                            phase = AppPhase.Ready,
                            online = true,
                            session = result.session,
                            baseUrl = result.session.baseUrl,
                            agents = result.roster.agents,
                        )
                    }
                is ResumeResult.Offline ->
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
                        session = session,
                        baseUrl = session.baseUrl,
                        token = "",
                        agents = roster.agents,
                        statusMessage = null,
                    )
                }
            }.onFailure { error ->
                tokenChars.fill('\u0000')
                mutableState.update {
                    it.copy(busy = false, token = "", error = humanError(error), statusMessage = null)
                }
            }
        }
    }

    fun onForeground() {
        if (state.value.phase != AppPhase.Ready || state.value.busy) return
        refresh(showProgress = false)
    }

    fun onBackground() {
        stopConversationStream()
    }

    fun refresh(showProgress: Boolean = true) {
        if (state.value.phase != AppPhase.Ready) return
        if (showProgress) mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    repository.retryOutbox()
                    repository.refreshRoster()
                }
            }.onSuccess { roster ->
                mutableState.update {
                    it.copy(
                        agents = roster.agents,
                        online = true,
                        busy = false,
                        statusMessage = null,
                    )
                }
                state.value.selectedAgent?.let(::openAgent)
            }.onFailure(::handleRuntimeFailure)
        }
    }

    fun openAgent(agent: AgentSummary) {
        if (state.value.enqueueing) return
        conversationJob?.cancel()
        stopConversationStream()
        val abandonedAttachments =
            state.value.attachments.takeIf { state.value.selectedAgent?.id != agent.id }.orEmpty()
        mutableState.update { current ->
            val sameConversation = current.selectedAgent?.id == agent.id
            current.copy(
                selectedAgent = agent,
                conversation = current.conversation.takeIf { sameConversation },
                outbox = current.outbox.takeIf { sameConversation }.orEmpty(),
                draft = current.draft.takeIf { sameConversation }.orEmpty(),
                attachments = current.attachments.takeIf { sameConversation }.orEmpty(),
                agentSection = current.agentSection.takeIf { sameConversation } ?: AgentSection.Results,
                briefs = current.briefs.takeIf { sameConversation }.orEmpty(),
                selectedBrief = current.selectedBrief.takeIf { sameConversation },
                selectedTurn = current.selectedTurn.takeIf { sameConversation },
                conversationDetail = current.conversationDetail.takeIf { sameConversation },
                selectedActivity = current.selectedActivity.takeIf { sameConversation },
                selectedToolExecution = current.selectedToolExecution.takeIf { sameConversation },
                workItems = current.workItems.takeIf { sameConversation }.orEmpty(),
                selectedWorkItem = current.selectedWorkItem.takeIf { sameConversation },
                workspaces = current.workspaces.takeIf { sameConversation }.orEmpty(),
                selectedWorkspace = current.selectedWorkspace.takeIf { sameConversation },
                workspaceDirectory = current.workspaceDirectory.takeIf { sameConversation },
                preparedArtifact = current.preparedArtifact.takeIf { sameConversation },
                busy = true,
                error = null,
            )
        }
        if (abandonedAttachments.isNotEmpty()) {
            viewModelScope.launch(Dispatchers.IO) {
                abandonedAttachments.forEach(repository::discardAttachment)
            }
        }
        conversationJob =
            viewModelScope.launch {
                runCatching {
                    withContext(Dispatchers.IO) { repository.conversation(agent) }
                }.onSuccess { bundle ->
                    mutableState.update {
                        it.copy(
                            conversation = bundle.snapshot,
                            outbox = bundle.outbox,
                            draft = bundle.draft,
                            busy = false,
                            online = true,
                        )
                    }
                    hydrateBriefs(agent, bundle.snapshot)
                    loadAgentWorkspace(agent)
                    startConversationStream(agent, bundle.snapshot.snapshotCursor)
                }.onFailure { error ->
                    val cached = runCatching {
                        withContext(Dispatchers.IO) { repository.cachedConversation(agent.id) }
                    }.getOrNull()
                    if (cached != null) {
                        mutableState.update {
                            it.copy(
                                conversation = cached.snapshot,
                                outbox = cached.outbox,
                                draft = cached.draft,
                                busy = false,
                                online = false,
                                statusMessage = "会话暂时离线，显示缓存",
                            )
                        }
                    } else {
                        handleRuntimeFailure(error)
                    }
                }
            }
    }

    fun closeConversation() {
        if (state.value.enqueueing) return
        conversationJob?.cancel()
        detailRefreshJob?.cancel()
        stopConversationStream()
        val abandonedAttachments = state.value.attachments
        mutableState.update {
            it.copy(
                selectedAgent = null,
                conversation = null,
                outbox = emptyList(),
                attachments = emptyList(),
                agentSection = AgentSection.Results,
                briefs = emptyMap(),
                selectedBrief = null,
                selectedTurn = null,
                conversationDetail = null,
                selectedActivity = null,
                selectedToolExecution = null,
                preparedArtifact = null,
                workItems = emptyList(),
                selectedWorkItem = null,
                workspaces = emptyList(),
                selectedWorkspace = null,
                workspaceDirectory = null,
                detailBusy = false,
                workItemsBusy = false,
                workspaceBusy = false,
                error = null,
            )
        }
        if (abandonedAttachments.isNotEmpty()) {
            viewModelScope.launch(Dispatchers.IO) {
                abandonedAttachments.forEach(repository::discardAttachment)
            }
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
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    repository.stageAttachment(
                        uri = uri,
                        preferredKind = preferredKind,
                        existingAttachments = before.attachments,
                        promptText = before.draft,
                    )
                }
            }.onSuccess { attachment ->
                mutableState.update { it.copy(attachments = it.attachments + attachment, error = null) }
            }.onFailure { error -> mutableState.update { it.copy(error = humanError(error)) } }
        }
    }

    fun removeAttachment(index: Int) {
        val removed = state.value.attachments.getOrNull(index)
        mutableState.update { current ->
            current.copy(attachments = current.attachments.filterIndexed { i, _ -> i != index })
        }
        removed?.let { viewModelScope.launch(Dispatchers.IO) { repository.discardAttachment(it) } }
    }

    fun send() {
        val current = state.value
        val agent = current.selectedAgent ?: return
        if (current.enqueueing) return
        if (current.draft.isBlank() && current.attachments.isEmpty()) return
        val text = current.draft
        val attachments = current.attachments
        val requestId = UUID.randomUUID().toString()
        val pendingDraftSave = draftSaveJobs[agent.id]
        val sentDraftRevision = draftRevision
        mutableState.update { it.copy(enqueueing = true, error = null) }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    pendingDraftSave?.join()
                    repository.enqueue(agent.id, text, attachments, requestId)
                }
            }.onSuccess { pending ->
                if (draftSaveJobs[agent.id] === pendingDraftSave) {
                    draftSaveJobs.remove(agent.id)
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
                withContext(Dispatchers.IO) {
                    repository.brief(agent.id, briefId) to repository.workItems(agent.id)
                }
            }.onSuccess { (brief, workItems) ->
                mutableState.update {
                    it.copy(
                        selectedBrief = brief,
                        briefs = it.briefs + (brief.id to brief),
                        workItems = workItems,
                        busy = false,
                    )
                }
            }.onFailure(::handleRuntimeFailure)
        }
    }

    fun closeBrief() = mutableState.update { it.copy(selectedBrief = null, preparedArtifact = null) }

    fun selectAgentSection(section: AgentSection) {
        mutableState.update {
            it.copy(
                agentSection = section,
                selectedTurn = null,
                conversationDetail = null,
                selectedActivity = null,
                selectedToolExecution = null,
                selectedWorkItem = null,
                preparedArtifact = null,
            )
        }
    }

    fun openTurn(turn: HolonConversationTurn) {
        val agent = state.value.selectedAgent ?: return
        mutableState.update {
            it.copy(
                selectedTurn = turn,
                conversationDetail = null,
                selectedActivity = null,
                selectedToolExecution = null,
                detailBusy = true,
                error = null,
            )
        }
        scheduleTurnDetailRefresh(agent, turn.id, delayMillis = 0)
    }

    fun closeTurn() {
        detailRefreshJob?.cancel()
        mutableState.update {
            it.copy(
                selectedTurn = null,
                conversationDetail = null,
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
        mutableState.update { it.copy(selectedWorkItem = item, workItemsBusy = true, error = null) }
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

    fun closeWorkItem() = mutableState.update { it.copy(selectedWorkItem = null) }

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

    fun selectMainDestination(destination: MainDestination) {
        mutableState.update { it.copy(mainDestination = destination) }
    }

    fun handleSystemBack(): Boolean {
        val current = state.value
        return when {
            current.preparedArtifact != null -> {
                clearPreparedArtifact()
                true
            }
            current.selectedActivity != null -> {
                closeActivity()
                true
            }
            current.selectedBrief != null -> {
                closeBrief()
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
                try {
                    while (isActive && state.value.selectedAgent?.id == agent.id) {
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
                                                it.copy(
                                                    conversation = bundle.snapshot,
                                                    selectedTurn =
                                                        selectedTurnId?.let { id ->
                                                            bundle.snapshot.turns.firstOrNull { turn -> turn.id == id }
                                                        } ?: it.selectedTurn,
                                                    outbox = bundle.outbox,
                                                    draft = bundle.draft,
                                                    online = true,
                                                    statusMessage = null,
                                                )
                                            }
                                        }
                                    }
                                    state.value.selectedTurn?.id?.let { turnId ->
                                        scheduleTurnDetailRefresh(agent, turnId)
                                    }
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
                        if (!reopenAfterReset) break
                    }
                } catch (_: CancellationException) {
                    // Closing the foreground-only stream is an expected lifecycle transition.
                } catch (error: Throwable) {
                    withContext(Dispatchers.Main) { handleRuntimeFailure(error) }
                }
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
                        mutableState.update { it.copy(conversationDetail = detail, detailBusy = false) }
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
        mutableState.update {
            it.copy(
                workItemsBusy = true,
                workspaceBusy = activeWorkspace != null,
                workspaces = activeWorkspace?.let(::listOf).orEmpty(),
                selectedWorkspace = activeWorkspace,
            )
        }
        activeWorkspace?.let { browseWorkspace(it, "") }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.workItems(agent.id) }
            }.onSuccess { items ->
                if (state.value.selectedAgent?.id == agent.id) {
                    mutableState.update { it.copy(workItems = items, workItemsBusy = false) }
                }
            }.onFailure { mutableState.update { it.copy(workItemsBusy = false) } }
        }
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.workspaces(agent.id) }
            }.onSuccess { workspaces ->
                if (state.value.selectedAgent?.id == agent.id) {
                    val workspace = workspaces.firstOrNull { it.isActive } ?: workspaces.firstOrNull()
                    val previous = state.value.selectedWorkspace
                    mutableState.update {
                        it.copy(
                            workspaces = workspaces,
                            selectedWorkspace = workspace,
                            workspaceBusy = workspace != null && workspace != previous,
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
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.browseWorkspace(workspace, path) }
            }.onSuccess { directory ->
                if (state.value.selectedWorkspace?.workspaceId == workspace.workspaceId &&
                    state.value.selectedWorkspace?.executionRootId == workspace.executionRootId
                ) {
                    mutableState.update { it.copy(workspaceDirectory = directory, workspaceBusy = false) }
                }
            }.onFailure { error ->
                mutableState.update { it.copy(workspaceBusy = false, error = humanError(error)) }
            }
        }
    }

    private fun handleRuntimeFailure(error: Throwable) {
        if (error is HolonHttpException && error.statusCode in setOf(401, 403)) {
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
