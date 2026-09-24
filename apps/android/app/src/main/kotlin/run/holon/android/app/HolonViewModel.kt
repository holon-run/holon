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
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonConversationStreamEvent
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonRosterSnapshot
import run.holon.android.sdk.HolonSseConnection
import run.holon.android.sdk.HolonWorkItemSnapshot
import run.holon.android.sdk.toConversationEvent

internal enum class AppPhase {
    Starting,
    SignedOut,
    Ready,
}

internal enum class MainDestination(val route: String, val label: String) {
    Recent("recent", "最近"),
    Agents("agents", "Agents"),
    Settings("settings", "设置"),
}

internal data class HolonUiState(
    val phase: AppPhase = AppPhase.Starting,
    val baseUrl: String = "",
    val token: String = "",
    val showToken: Boolean = false,
    val allowInsecureHttp: Boolean = false,
    val busy: Boolean = false,
    val enqueueing: Boolean = false,
    val online: Boolean = false,
    val session: ActiveSession? = null,
    val agents: List<AgentSummary> = emptyList(),
    val selectedAgent: AgentSummary? = null,
    val conversation: HolonConversationSnapshot? = null,
    val outbox: List<OutboxEntity> = emptyList(),
    val draft: String = "",
    val attachments: List<StagedAttachment> = emptyList(),
    val selectedBrief: HolonBrief? = null,
    val workItems: List<HolonWorkItemSnapshot> = emptyList(),
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
            agents.filter {
                search.isBlank() ||
                    it.displayName.contains(search, ignoreCase = true) ||
                    it.id.contains(search, ignoreCase = true)
            }.sortedBy { it.displayName.lowercase() }
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
                selectedBrief = current.selectedBrief.takeIf { sameConversation },
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
        stopConversationStream()
        val abandonedAttachments = state.value.attachments
        mutableState.update {
            it.copy(
                selectedAgent = null,
                conversation = null,
                outbox = emptyList(),
                attachments = emptyList(),
                selectedBrief = null,
                preparedArtifact = null,
                workItems = emptyList(),
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
                mutableState.update { it.copy(selectedBrief = brief, workItems = workItems, busy = false) }
            }.onFailure(::handleRuntimeFailure)
        }
    }

    fun closeBrief() = mutableState.update { it.copy(selectedBrief = null, workItems = emptyList()) }

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

    fun clearPreparedArtifact() = mutableState.update { it.copy(preparedArtifact = null) }

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
                                if (change is HolonConversationStreamEvent.Checkpoint ||
                                    change is HolonConversationStreamEvent.ResetRequired
                                ) {
                                    val bundle = repository.conversation(agent)
                                    cursor = bundle.snapshot.snapshotCursor
                                    withContext(Dispatchers.Main) {
                                        if (state.value.selectedAgent?.id == agent.id) {
                                            mutableState.update {
                                                it.copy(
                                                    conversation = bundle.snapshot,
                                                    outbox = bundle.outbox,
                                                    draft = bundle.draft,
                                                    online = true,
                                                    statusMessage = null,
                                                )
                                            }
                                        }
                                    }
                                    if (change is HolonConversationStreamEvent.ResetRequired) {
                                        reopenAfterReset = true
                                        break
                                    }
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

    private fun stopConversationStream() {
        conversationStream?.close()
        conversationStream = null
        conversationStreamJob?.cancel()
        conversationStreamJob = null
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
