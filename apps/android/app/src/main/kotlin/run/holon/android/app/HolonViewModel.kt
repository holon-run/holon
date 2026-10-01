package run.holon.android.app

import android.app.Application
import android.content.Context
import android.net.Uri
import androidx.core.content.FileProvider
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
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonAgentEvent
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
import run.holon.android.sdk.HolonWorkspace
import run.holon.android.sdk.HolonWorkspaceDirectory
import run.holon.android.sdk.toConversationEvent

internal fun HolonHttpException.isStaleAgentEventCursor(): Boolean =
    statusCode == 404 && apiError?.code == "cursor_not_found"

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

internal data class HolonUiState(
    val phase: AppPhase = AppPhase.Starting,
    val baseUrl: String = "",
    val token: String = "",
    val showToken: Boolean = false,
    val allowInsecureHttp: Boolean = false,
    val pendingPairing: ScannedPairing? = null,
    val mainDestination: MainDestination = MainDestination.Agents,
    val busy: Boolean = false,
    val enqueueing: Boolean = false,
    val stagingAttachment: Boolean = false,
    val abortingRun: Boolean = false,
    val online: Boolean = false,
    val lastSyncedAt: Long? = null,
    val session: ActiveSession? = null,
    val networkProfiles: List<NetworkProfile> = emptyList(),
    val switchingNetworkId: String? = null,
    val agents: List<AgentSummary> = emptyList(),
    val briefReadStates: Map<String, HolonBriefReadState> = emptyMap(),
    val briefReadStatesLoaded: Boolean = false,
    val readBriefIds: Map<String, String> = emptyMap(),
    val readBriefsLoaded: Boolean = false,
    val selectedAgent: AgentSummary? = null,
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
    val pendingShare: PendingAgentShare? = null,
    val queuedShares: List<PendingAgentShare> = emptyList(),
    val shareSending: Boolean = false,
    val shareError: String? = null,
    val agentSection: AgentSection = AgentSection.Results,
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
                    .thenByDescending {
                        if (briefReadStatesLoaded) {
                            it.unreadCount(briefReadStates) > 0
                        } else {
                            readBriefsLoaded && it.hasUnreadBrief(readBriefIds)
                        }
                    }
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

internal fun isCurrentLiveSync(
    foreground: Boolean,
    phase: AppPhase,
    expectedGeneration: Long,
    currentGeneration: Long,
): Boolean =
    foreground && phase == AppPhase.Ready && expectedGeneration == currentGeneration

private const val SESSION_KEEPALIVE_INTERVAL_MILLIS = 15 * 60 * 1000L

internal class SessionResetBarrier(private val scope: kotlinx.coroutines.CoroutineScope) {
    private var pending: Job? = null
    private var activeTransitions = 0

    fun beginTransition() {
        synchronized(this) {
            activeTransitions += 1
        }
    }

    fun endTransition() {
        synchronized(this) {
            check(activeTransitions > 0)
            activeTransitions -= 1
        }
    }

    fun schedule(reset: suspend () -> Unit): Job {
        synchronized(this) {
            if (activeTransitions > 0) return scope.launch {}
        }
        val previous = pending
        val next =
            scope.launch {
                previous?.join()
                runCatching { reset() }
            }
        pending = next
        return next
    }

    suspend fun await() {
        while (true) {
            val current = pending ?: return
            current.join()
            if (pending === current) {
                pending = null
                return
            }
        }
    }
}

internal class AppContainer(context: Context) {
    private val database = HolonDatabase.create(context)
    val traceRecorder = TraceRecorder(context)
    val repository =
        HolonRepository(
            context = context,
            sessionStore = createSessionStore(context),
            preferences = HostPreferences(context),
            dao = database.holonDao(),
            traceRecorder = traceRecorder,
        )
}

internal class HolonViewModel(
    application: Application,
    private val repository: HolonRepository,
    internal val traceRecorder: TraceRecorder,
) : AndroidViewModel(application) {
    private val mutableState = MutableStateFlow(HolonUiState(baseUrl = defaultBaseUrl()))
    val state: StateFlow<HolonUiState> = mutableState.asStateFlow()
    private var conversationJob: Job? = null
    private var conversationStreamJob: Job? = null
    private var conversationStream: HolonSseConnection? = null
    private var globalEventStreamJob: Job? = null
    // SSE readers block threads; keep them off the pool used by foreground sends and file staging.
    private val eventStreamIo = Dispatchers.IO.limitedParallelism(64)
    private val agentEventStreamJobs = ConcurrentHashMap<String, Job>()
    private val agentEventCursors = ConcurrentHashMap<String, Long>()
    private val staleCursorRecoveryMutex = Mutex()
    @Volatile private var liveEventLogEpoch: String? = null
    private var liveRosterRefreshJob: Job? = null
    private var detailRefreshJob: Job? = null
    private var workspaceBrowseJob: Job? = null
    private var artifactJob: Job? = null
    private var workspaceBrowseGeneration = 0L
    private var workspaceBrowseRequest: WorkspaceBrowseRequest? = null
    private var refreshJob: Job? = null
    private val sessionResetBarrier = SessionResetBarrier(viewModelScope)
    private var sessionTransitionGeneration = 0L
    private var briefReadStateScope: String? = null
    private val rosterRefresh = RosterRefreshRequest(viewModelScope) {
        withContext(Dispatchers.IO) { repository.refreshSessionAndRoster() }
    }
    private val briefReadStateLoader = BriefReadStateLoader(
        scope = viewModelScope,
        read = {
            val scopeKey = state.value.session?.scopeKey
            val agents = state.value.agents
            val snapshot = withContext(Dispatchers.IO) {
                try {
                    BriefReadSnapshot.Server(repository.briefReadStates())
                } catch (error: Throwable) {
                    if (!isBriefReadStateUnsupported(error)) throw error
                    BriefReadSnapshot.Legacy(repository.readBriefIds(agents))
                }
            }
            if (scopeKey != state.value.session?.scopeKey) throw CancellationException("Session changed")
            snapshot
        },
        onLoaded = { snapshot -> mutableState.update { it.withBriefReadSnapshot(snapshot) } },
        onFailure = { error ->
            if (error.isAuthenticationFailure() || error is SessionScopeChangedException) {
                handleRuntimeFailure(error)
            } else {
                mutableState.update { it.withBriefReadFailure(error) }
            }
        },
    )
    private var sessionKeepAliveJob: Job? = null
    @Volatile private var foreground = true
    private val draftSaveJobs = mutableMapOf<String, Job>()
    private val composerSaveJobs = mutableMapOf<String, Job>()
    private var draftRevision = 0L
    @Volatile private var liveSyncGeneration = 0L
    private var briefScope: String? = null
    private val readingCache = linkedMapOf<String, ConversationReadingState>()
    private fun rememberConversation() {
        val current = state.value
        val agent = current.selectedAgent ?: return
        val key = "${current.session?.scopeKey}:${agent.id}"
        readingCache.remove(key)
        readingCache[key] = ConversationReadingState.from(current)
        while (readingCache.size > 4) readingCache.remove(readingCache.keys.first())
    }
    private val briefLoader = BriefLoader(
        scope = viewModelScope,
        read = { id: String ->
            val agent = state.value.selectedAgent ?: error("No conversation")
            withContext(Dispatchers.IO) { repository.brief(agent.id, id) }
        },
        onLoading = { id -> mutableState.update { it.copy(briefLoads = it.briefLoads + (id to BriefLoadState.Loading)) } },
        onLoaded = { id, brief -> mutableState.update { it.copy(briefs = it.briefs + (id to brief), briefLoads = it.briefLoads - id) } },
        onFailure = { id, error ->
            if (error.isAuthenticationFailure() || error is SessionScopeChangedException) {
                handleRuntimeFailure(error)
            } else {
                mutableState.update { it.copy(briefLoads = it.briefLoads + (id to BriefLoadState.Failed(humanError(error)))) }
            }
        },
    )

    init {
        viewModelScope.launch {
            val (profiles, result) =
                withContext(Dispatchers.IO) {
                    repository.networkProfiles() to repository.resume()
                }
            when (result) {
                ResumeResult.NoSession ->
                    mutableState.update { it.copy(phase = AppPhase.SignedOut, networkProfiles = profiles) }
                is ResumeResult.Ready -> {
                    mutableState.update {
                        it.copy(
                            phase = AppPhase.Ready,
                            online = true,
                            lastSyncedAt = System.currentTimeMillis(),
                            session = result.session,
                            baseUrl = result.session.baseUrl,
                            networkProfiles = profiles,
                            agents = result.roster.agents,
                        )
                    }
                    loadBriefReadStates()
                    startLiveSync(result.roster.agents, result.roster.eventLogEpoch)
                    viewModelScope.launch(Dispatchers.IO) { runCatching { repository.retryOutbox() } }
                }
                is ResumeResult.Offline -> {
                    mutableState.update {
                        it.copy(
                            phase = AppPhase.Ready,
                            online = false,
                            session = result.session,
                            baseUrl = result.session.baseUrl,
                            networkProfiles = profiles,
                            agents = result.cached.map(AgentProjectionEntity::toAgentSummary),
                            statusMessage = "当前离线，显示上次同步内容",
                        )
                    }
                    loadBriefReadStates()
                    refresh(showProgress = false)
                }
                is ResumeResult.Incompatible ->
                    mutableState.update {
                        it.copy(
                            phase = AppPhase.SignedOut,
                            networkProfiles = profiles,
                            baseUrl = result.baseUrl,
                            error = result.message,
                        )
                    }
            }
        }
    }

    fun setBaseUrl(value: String) =
        mutableState.update {
            it.copy(baseUrl = value, allowInsecureHttp = false, pendingPairing = null, error = null)
        }

    fun applyScannedAddress(value: String) {
        if (state.value.busy) return
        runCatching {
            val uri = java.net.URI(value.trim())
            if (uri.path == "/login" || uri.rawFragment != null) {
                parseScannedPairing(value)
            } else {
                normalizeScannedAddress(value)
            }
        }
            .onSuccess { scanned ->
                when (scanned) {
                    is ScannedPairing -> mutableState.update { it.copy(pendingPairing = scanned, error = null) }
                    is String -> setBaseUrl(scanned)
                }
            }
            .onFailure { error ->
                mutableState.update {
                    it.copy(pendingPairing = null, error = error.message ?: "二维码地址无效")
                }
            }
    }

    fun cancelPairing() = mutableState.update { it.copy(pendingPairing = null) }

    fun confirmPairing() {
        val pairing = state.value.pendingPairing ?: return
        mutableState.update { it.copy(pendingPairing = null) }
        login(pairing)
    }

    fun reportScanFailure() {
        mutableState.update { it.copy(error = "二维码读取失败，请重试或手动输入地址") }
    }
    fun setToken(value: String) = mutableState.update { it.copy(token = value, error = null) }
    fun toggleToken() = mutableState.update { it.copy(showToken = !it.showToken) }
    fun setAllowInsecureHttp(value: Boolean) =
        mutableState.update { it.copy(allowInsecureHttp = value, error = null) }
    fun setSearch(value: String) = mutableState.update { it.copy(search = value) }

    fun beginAddNetwork() {
        val current = state.value
        if (current.phase != AppPhase.Ready || current.busy || current.session == null) return
        invalidateLiveSync()
        mutableState.update(HolonUiState::forAddingNetwork)
    }

    fun cancelAddNetwork() {
        val current = state.value
        if (current.phase != AppPhase.AddingNetwork || current.busy) return
        mutableState.update(HolonUiState::afterCancelAddingNetwork)
        if (foreground) {
            startLiveSync(state.value.agents)
            refresh(showProgress = false)
        }
    }

    fun switchNetwork(networkId: String) {
        val before = state.value
        val profile = before.networkProfiles.firstOrNull { it.networkId == networkId } ?: return
        if (before.busy || before.session?.networkId == networkId) return
        val generation = invalidateLiveSync()
        sessionTransitionGeneration += 1
        mutableState.update {
            it.copy(
                busy = true,
                switchingNetworkId = networkId,
                error = null,
                statusMessage = "正在切换到 ${profile.displayName}…",
                selectedAgent = null,
                conversation = null,
                olderTurns = emptyList(),
                historyBeforeCursor = null,
                agents = emptyList(),
            )
        }
        sessionResetBarrier.beginTransition()
        viewModelScope.launch {
            try {
                sessionResetBarrier.await()
            runCatching {
                withContext(Dispatchers.IO) {
                    repository.switchNetwork(networkId) to repository.networkProfiles()
                }
            }.onSuccess { (result, profiles) ->
                if (generation != liveSyncGeneration) {
                    return@onSuccess
                }
                when (result) {
                    ResumeResult.NoSession ->
                        mutableState.update {
                            it.copy(
                                phase = AppPhase.SignedOut,
                                busy = false,
                                switchingNetworkId = null,
                                networkProfiles = profiles,
                                baseUrl = profile.baseUrl,
                                token = "",
                                allowInsecureHttp = false,
                                session = null,
                                statusMessage = null,
                            )
                        }
                    is ResumeResult.Ready -> {
                        mutableState.update {
                            it.copy(
                                phase = AppPhase.Ready,
                                busy = false,
                                switchingNetworkId = null,
                                online = true,
                                lastSyncedAt = System.currentTimeMillis(),
                                networkProfiles = profiles,
                                session = result.session,
                                baseUrl = result.session.baseUrl,
                                agents = result.roster.agents,
                                statusMessage = null,
                                briefReadStates = emptyMap(),
                                briefReadStatesLoaded = false,
                                readBriefIds = emptyMap(),
                                readBriefsLoaded = false,
                            )
                        }
                        loadBriefReadStates()
                        if (foreground) {
                            startLiveSync(result.roster.agents, result.roster.eventLogEpoch, generation)
                        }
                    }
                    is ResumeResult.Offline ->
                        mutableState.update {
                            it.copy(
                                phase = AppPhase.Ready,
                                busy = false,
                                switchingNetworkId = null,
                                online = false,
                                networkProfiles = profiles,
                                session = result.session,
                                baseUrl = result.session.baseUrl,
                                agents = result.cached.map(AgentProjectionEntity::toAgentSummary),
                                statusMessage = "当前离线，显示上次同步内容",
                                briefReadStates = emptyMap(),
                                briefReadStatesLoaded = false,
                                readBriefIds = emptyMap(),
                                readBriefsLoaded = false,
                            )
                        }
                    is ResumeResult.Incompatible ->
                        mutableState.update {
                            it.copy(
                                phase = AppPhase.SignedOut,
                                busy = false,
                                switchingNetworkId = null,
                                networkProfiles = profiles,
                                baseUrl = result.baseUrl,
                                allowInsecureHttp = false,
                                session = null,
                                error = result.message,
                                statusMessage = null,
                            )
                        }
                }
            }.onFailure { error ->
                if (generation == liveSyncGeneration) {
                    mutableState.update {
                        it.copy(busy = false, switchingNetworkId = null, error = humanError(error), statusMessage = null)
                    }
                }
            }
            } finally {
                sessionResetBarrier.endTransition()
            }
        }
    }

    private fun loadBriefReadStates() {
        val scopeKey = state.value.session?.scopeKey ?: return
        if (scopeKey != briefReadStateScope) {
            briefReadStateLoader.reset()
            briefReadStateScope = scopeKey
        }
        if (foreground) briefReadStateLoader.request()
    }

    fun markBriefRead(agentId: String, readThroughEventSeq: Long) {
        val current = state.value
        if (current.selectedAgent?.id != agentId) return
        val latestBriefId = current.selectedAgent?.latestBrief?.briefId ?: return
        val existing = current.briefReadStates[agentId]
        if (
            (current.briefReadStatesLoaded && existing != null && existing.readThroughEventSeq >= readThroughEventSeq) ||
            (!current.briefReadStatesLoaded && current.readBriefsLoaded && current.readBriefIds[agentId] == latestBriefId)
        ) {
            return
        }
        val scopeKey = current.session?.scopeKey ?: return
        viewModelScope.launch {
            runCatching { withContext(Dispatchers.IO) { repository.markBriefRead(agentId, readThroughEventSeq) } }
                .onSuccess { result ->
                    if (state.value.session?.scopeKey == scopeKey) {
                        mutableState.update {
                            it.copy(
                                briefReadStates = it.briefReadStates + (agentId to result.state),
                                briefReadStatesLoaded = true,
                            )
                        }
                    }
                }
                .onFailure { error ->
                    if (state.value.session?.scopeKey == scopeKey) {
                        if (isBriefReadStateUnsupported(error)) {
                            viewModelScope.launch(Dispatchers.IO) {
                                repository.markBriefRead(agentId, latestBriefId)
                            }
                            mutableState.update {
                                it.copy(
                                    readBriefIds = it.readBriefIds + (agentId to latestBriefId),
                                    readBriefsLoaded = true,
                                )
                            }
                        } else {
                            mutableState.update { it.copy(error = "无法保存已读状态：${humanError(error)}") }
                        }
                    }
                }
        }
    }

    fun login() {
        login(null)
    }

    private fun login(pairing: ScannedPairing?) {
        val before = state.value
        if (before.busy || before.phase !in setOf(AppPhase.SignedOut, AppPhase.AddingNetwork)) return
        if (pairing == null && before.token.isBlank()) {
            mutableState.update { it.copy(error = "请输入 token") }
            return
        }
        val tokenChars = pairing?.let { charArrayOf() } ?: before.token.toCharArray()
        sessionTransitionGeneration += 1
        mutableState.update { it.copy(busy = true, error = null, statusMessage = "正在安全登录…") }
        sessionResetBarrier.beginTransition()
        viewModelScope.launch {
            try {
                sessionResetBarrier.await()
            runCatching {
                withContext(Dispatchers.IO) {
                    val (session, roster) =
                        repository.login(
                            pairing?.address ?: before.baseUrl,
                            tokenChars,
                            pairing?.address?.startsWith("http://") ?: before.allowInsecureHttp,
                            pairing?.ticket,
                        )
                    Triple(session, roster, repository.networkProfiles())
                }
            }.onSuccess { (session, roster, profiles) ->
                mutableState.value =
                    HolonUiState(
                        phase = AppPhase.Ready,
                        pendingShare = before.pendingShare,
                        queuedShares = before.queuedShares,
                        mainDestination =
                            if (before.phase == AppPhase.AddingNetwork) MainDestination.Settings
                            else MainDestination.Agents,
                        online = true,
                        lastSyncedAt = System.currentTimeMillis(),
                        session = session,
                        baseUrl = session.baseUrl,
                        networkProfiles = profiles,
                        agents = roster.agents,
                    )
                loadBriefReadStates()
                startLiveSync(roster.agents, roster.eventLogEpoch)
            }.onFailure { error ->
                tokenChars.fill('\u0000')
                mutableState.update {
                    it.copy(
                        busy = false,
                        token = "",
                        error = if (pairing != null) pairingHumanError(error) else humanError(error),
                        statusMessage = null,
                    )
                }
            }
            } finally {
                sessionResetBarrier.endTransition()
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

    private suspend fun loadRoster(generation: Long = liveSyncGeneration): Pair<ActiveSession, HolonRosterSnapshot> {
        if (generation != liveSyncGeneration) throw CancellationException("Session changed")
        return rosterRefresh.load().await().also {
            if (generation != liveSyncGeneration) throw CancellationException("Session changed")
        }
    }

    fun refresh(showProgress: Boolean = true) {
        if (state.value.phase != AppPhase.Ready) return
        if (refreshJob?.isActive == true) return
        val generation = liveSyncGeneration
        if (showProgress) mutableState.update { it.copy(busy = true, error = null) }
        refreshJob = viewModelScope.launch {
            runCatching {
                loadRoster(generation)
            }.onSuccess { (session, roster) ->
                if (generation != liveSyncGeneration) return@onSuccess
                mutableState.update {
                    val scopeChanged = it.session?.scopeKey != session.scopeKey
                    it.copy(
                        session = session,
                        agents = roster.agents,
                        briefReadStates = if (scopeChanged) emptyMap() else it.briefReadStates,
                        briefReadStatesLoaded = if (scopeChanged) false else it.briefReadStatesLoaded,
                        readBriefIds = if (scopeChanged) emptyMap() else it.readBriefIds,
                        readBriefsLoaded = if (scopeChanged) false else it.readBriefsLoaded,
                        online = true,
                        lastSyncedAt = System.currentTimeMillis(),
                        busy = false,
                        statusMessage = null,
                    )
                }
                loadBriefReadStates()
                startLiveSync(roster.agents, roster.eventLogEpoch)
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
                if (generation != liveSyncGeneration) return@onFailure
                handleRuntimeFailure(error)
                if (state.value.phase == AppPhase.Ready && foreground) {
                    state.value.selectedAgent?.let { agent ->
                        startConversationStream(agent, state.value.conversation?.snapshotCursor)
                    }
                }
            }
        }
    }

    private fun startLiveSync(
        agents: List<AgentSummary>,
        eventLogEpoch: String? = null,
        generation: Long = liveSyncGeneration,
    ) {
        if (!isCurrentLiveSync(foreground, state.value.phase, generation, liveSyncGeneration)) return
        eventLogEpoch?.let { liveEventLogEpoch = it }
        startSessionKeepAlive(generation)
        if (globalEventStreamJob?.isActive != true) {
            globalEventStreamJob =
                viewModelScope.launch(eventStreamIo) {
                    runCatching {
                        while (
                            isActive &&
                                isCurrentLiveSync(
                                    foreground,
                                    state.value.phase,
                                    generation,
                                    liveSyncGeneration,
                                )
                        ) {
                            repository.reconnectingRosterHints(
                                policy = SseReconnectPolicy(maxAttempts = 8),
                            ).forEach {
                                if (
                                    isActive &&
                                        isCurrentLiveSync(
                                            foreground,
                                            state.value.phase,
                                            generation,
                                            liveSyncGeneration,
                                        )
                                ) {
                                    scheduleLiveRosterRefresh()
                                }
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
                viewModelScope.launch(eventStreamIo) {
                    runCatching {
                        val persistedState = repository.syncState(agent.id)
                        var persistedCursor = persistedState?.eventCursor
                        var streamEpoch = liveEventLogEpoch
                        if (
                            persistedState?.eventLogEpoch != null &&
                                streamEpoch != null &&
                                persistedState.eventLogEpoch != streamEpoch
                        ) {
                            repository.resetAgentEventCursor(agent.id, streamEpoch)
                            persistedCursor = null
                            agentEventCursors.remove(agent.id)
                        }
                        persistedCursor?.let { agentEventCursors[agent.id] = it }
                        while (
                            isActive &&
                                isCurrentLiveSync(
                                    foreground,
                                    state.value.phase,
                                    generation,
                                    liveSyncGeneration,
                                )
                        ) {
                            val currentEpoch = liveEventLogEpoch
                            if (streamEpoch != null && currentEpoch != null && streamEpoch != currentEpoch) {
                                repository.resetAgentEventCursor(agent.id, currentEpoch)
                                persistedCursor = null
                                streamEpoch = currentEpoch
                                agentEventCursors.remove(agent.id)
                            }
                            try {
                                for (event in repository.reconnectingAgentEvents(
                                    agentId = agent.id,
                                    afterSeq = persistedCursor,
                                    policy = SseReconnectPolicy(maxAttempts = 8),
                                )) {
                                    if (
                                        !isActive ||
                                            !isCurrentLiveSync(
                                                foreground,
                                                state.value.phase,
                                                generation,
                                                liveSyncGeneration,
                                            )
                                    ) {
                                        break
                                    }
                                    if (
                                        liveEventLogEpoch != null &&
                                            event.eventLogEpoch != liveEventLogEpoch
                                    ) {
                                        repository.resetAgentEventCursor(agent.id, event.eventLogEpoch)
                                        persistedCursor = null
                                        streamEpoch = event.eventLogEpoch
                                        agentEventCursors.remove(agent.id)
                                        recoverLiveRosterAfterStaleCursor()
                                        break
                                    }
                                    persistedCursor = event.eventSeq
                                    agentEventCursors[agent.id] = event.eventSeq
                                    repository.saveAgentEventCursor(agent.id, event)
                                    scheduleLiveRosterRefresh()
                                }
                            } catch (error: HolonHttpException) {
                                if (!error.isStaleAgentEventCursor()) throw error
                                persistedCursor = null
                                agentEventCursors.remove(agent.id)
                                repository.resetAgentEventCursor(agent.id, liveEventLogEpoch)
                                streamEpoch = recoverLiveRosterAfterStaleCursor() ?: liveEventLogEpoch
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

    private suspend fun recoverLiveRosterAfterStaleCursor(): String? =
        staleCursorRecoveryMutex.withLock {
            if (!foreground || state.value.phase != AppPhase.Ready) return@withLock liveEventLogEpoch
            val generation = liveSyncGeneration
            runCatching {
                loadRoster(generation)
            }.onSuccess { (session, roster) ->
                if (generation != liveSyncGeneration) return@onSuccess
                liveEventLogEpoch = roster.eventLogEpoch
                mutableState.update {
                    it.copy(
                        session = session,
                        agents = roster.agents,
                        online = true,
                        lastSyncedAt = System.currentTimeMillis(),
                        statusMessage = null,
                    )
                }
                loadBriefReadStates()
                startLiveSync(roster.agents, roster.eventLogEpoch, generation)
            }.onFailure(::handleRuntimeFailure).getOrNull()?.second?.eventLogEpoch
        }

    private fun stopLiveSync() {
        briefReadStateLoader.reset()
        sessionKeepAliveJob?.cancel()
        sessionKeepAliveJob = null
        globalEventStreamJob?.cancel()
        globalEventStreamJob = null
        agentEventStreamJobs.values.forEach(Job::cancel)
        agentEventStreamJobs.clear()
        liveRosterRefreshJob?.cancel()
        liveRosterRefreshJob = null
    }

    private fun startSessionKeepAlive(generation: Long) {
        if (sessionKeepAliveJob?.isActive == true) return
        sessionKeepAliveJob =
            viewModelScope.launch(eventStreamIo) {
                while (
                    isActive &&
                        isCurrentLiveSync(
                            foreground,
                            state.value.phase,
                            generation,
                            liveSyncGeneration,
                        )
                ) {
                    try {
                        delay(SESSION_KEEPALIVE_INTERVAL_MILLIS)
                        if (
                            !isActive ||
                                !isCurrentLiveSync(
                                    foreground,
                                    state.value.phase,
                                    generation,
                                    liveSyncGeneration,
                                )
                        ) {
                            break
                        }
                        withContext(Dispatchers.IO) {
                            repository.keepSessionAlive()
                        }
                    } catch (_: CancellationException) {
                        break
                    } catch (error: Throwable) {
                        if (error.isAuthenticationFailure() || error is SessionScopeChangedException) {
                            withContext(Dispatchers.Main) { handleRuntimeFailure(error) }
                            break
                        }
                    }
                }
            }
    }

    private fun invalidateLiveSync(): Long {
        liveSyncGeneration += 1
        briefLoader.reset()
        briefScope = null
        detailRefreshJob?.cancel()
        readingCache.clear()
        artifactJob?.cancel()
        stopConversationStream()
        stopLiveSync()
        conversationJob?.cancel()
        conversationJob = null
        refreshJob?.cancel()
        refreshJob = null
        briefReadStateLoader.reset()
        briefReadStateScope = null
        rosterRefresh.reset()
        return liveSyncGeneration
    }

    private fun scheduleLiveRosterRefresh() {
        val generation = liveSyncGeneration
        if (!foreground || state.value.phase != AppPhase.Ready) return
        liveRosterRefreshJob?.cancel()
        liveRosterRefreshJob =
            viewModelScope.launch {
                delay(250)
                runCatching { loadRoster(generation) }
                    .onSuccess { (session, roster) ->
                        if (!isCurrentLiveSync(foreground, state.value.phase, generation, liveSyncGeneration)) {
                            return@onSuccess
                        }
                        mutableState.update {
                            it.copy(
                                session = session,
                                agents = roster.agents,
                                online = true,
                                lastSyncedAt = System.currentTimeMillis(),
                                statusMessage = null,
                            )
                        }
                        loadBriefReadStates()
                        startLiveSync(roster.agents, roster.eventLogEpoch, generation)
                    }
                    .onFailure { error ->
                        if (isCurrentLiveSync(foreground, state.value.phase, generation, liveSyncGeneration)) {
                            handleRuntimeFailure(error)
                        }
                    }
            }
    }

    fun openAgent(agent: AgentSummary) {
        if (state.value.enqueueing) return
        val sameAgentBeforeLoad = state.value.selectedAgent?.id == agent.id
        if (!sameAgentBeforeLoad) rememberConversation()
        val savedReading = if (!sameAgentBeforeLoad) readingCache["${state.value.session?.scopeKey}:${agent.id}"] else null
        if (!sameAgentBeforeLoad) {
            artifactJob?.cancel()
            briefLoader.reset()
            briefScope = null
            detailRefreshJob?.cancel()
        }
        conversationJob?.cancel()
        stopConversationStream()
        mutableState.update { original ->
            val current = savedReading?.restore(original, agent) ?: original
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
                briefLoads = current.briefLoads.takeIf { sameConversation }.orEmpty(),
                fullScreenTurn = sameConversation && current.fullScreenTurn,
                selectedBrief = current.selectedBrief.takeIf { sameConversation },
                briefOriginWork = current.briefOriginWork.takeIf { sameConversation },
                workOriginBrief = current.workOriginBrief.takeIf { sameConversation },
                planFile = current.planFile.takeIf { sameConversation },
                fileLinkOrigin = current.fileLinkOrigin.takeIf { sameConversation },
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
                            briefs = it.briefs.takeIf { keepHistory }.orEmpty(),
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
                    if (error is CancellationException) return@onFailure
                    if ((error.isAuthenticationFailure()) ||
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
        artifactJob?.cancel()
        rememberConversation()
        briefLoader.reset()
        briefScope = null
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
                briefLoads = emptyMap(),
                fullScreenTurn = false,
                briefOriginWork = null,
                workOriginBrief = null,
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

    fun loadModelCatalog(refresh: Boolean = false) {
        if (state.value.modelBusy) return
        mutableState.update { it.copy(modelBusy = true, modelError = null) }
        viewModelScope.launch {
            runCatching {
                repository.modelCatalog(refresh)
            }.onSuccess { catalog ->
                mutableState.update { it.copy(modelCatalog = catalog, modelBusy = false) }
            }.onFailure { error ->
                mutableState.update {
                    it.copy(modelBusy = false, modelError = humanError(error))
                }
            }
        }
    }

    fun setAgentModel(model: String, reasoningEffort: String?) {
        val agent = state.value.selectedAgent ?: return
        if (state.value.modelBusy) return
        mutableState.update { it.copy(modelBusy = true, modelError = null) }
        viewModelScope.launch {
            runCatching {
                repository.setAgentModel(agent.id, model, reasoningEffort)
            }.onSuccess { result ->
                updateAgentModel(agent.id, result.effectiveModel ?: model, "agent_override", model, reasoningEffort)
                mutableState.update { it.copy(modelBusy = false, statusMessage = "模型已更新") }
            }.onFailure { error ->
                mutableState.update {
                    it.copy(modelBusy = false, modelError = humanError(error))
                }
            }
        }
    }

    fun clearAgentModel() {
        val agent = state.value.selectedAgent ?: return
        if (state.value.modelBusy) return
        mutableState.update { it.copy(modelBusy = true, modelError = null) }
        viewModelScope.launch {
            runCatching {
                repository.clearAgentModel(agent.id)
            }.onSuccess { result ->
                updateAgentModel(agent.id, result.effectiveModel ?: agent.effectiveModel, "runtime_default", null, null)
                mutableState.update { it.copy(modelBusy = false, statusMessage = "已恢复 Auto 模型") }
            }.onFailure { error ->
                mutableState.update {
                    it.copy(modelBusy = false, modelError = humanError(error))
                }
            }
        }
    }

    private fun updateAgentModel(agentId: String, effectiveModel: String, source: String, override: String?, effort: String?) {
        mutableState.update { current ->
            val updated = current.agents.map { agent ->
                if (agent.id == agentId) agent.copy(
                    effectiveModel = effectiveModel,
                    modelSource = source,
                    overrideModel = override,
                    overrideReasoningEffort = effort,
                ) else agent
            }
            current.copy(
                agents = updated,
                selectedAgent = updated.firstOrNull { it.id == agentId } ?: current.selectedAgent,
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

    fun offerShare(share: PendingAgentShare) {
        mutableState.update {
            if (it.pendingShare == null) it.copy(pendingShare = share, shareError = null)
            else it.copy(queuedShares = it.queuedShares + share)
        }
    }

    fun shareTraceWithAgent() {
        val session = state.value.session ?: return
        if (state.value.phase != AppPhase.Ready) return
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    val file = traceRecorder.export(TraceScope.Network(session.networkId))
                    val context = getApplication<Application>()
                    val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
                    PendingAgentShare(
                        text = ui("请分析附件中的 Android Trace。"),
                        files = listOf(SharedFile(uri, file.name, file.length(), "application/x-ndjson")),
                        fromTrace = true,
                        sourceScopeKey = session.scopeKey,
                    )
                }
            }.onSuccess { share ->
                if (state.value.session?.scopeKey == session.scopeKey) offerShare(share)
            }
                .onFailure { error -> mutableState.update { it.copy(error = humanError(error)) } }
        }
    }

    fun updateShareText(text: String) {
        mutableState.update { it.copy(pendingShare = it.pendingShare?.copy(text = text), shareError = null) }
    }

    fun dismissShare() {
        if (state.value.shareSending) return
        mutableState.update(::nextShare)
    }

    private fun nextShare(state: HolonUiState): HolonUiState =
        state.copy(
            pendingShare = state.queuedShares.firstOrNull(),
            queuedShares = state.queuedShares.drop(1),
            shareSending = false,
            shareError = null,
        )

    fun sendShare(agent: AgentSummary) {
        val before = state.value
        val share = before.pendingShare ?: return
        val session = before.session ?: return
        if (before.phase != AppPhase.Ready || before.shareSending || agent.id !in before.agents.map(AgentSummary::id)) return
        if (share.sourceScopeKey != null && share.sourceScopeKey != session.scopeKey) {
            mutableState.update { it.copy(shareError = "此 Trace 属于其他连接，请重新导出") }
            return
        }
        mutableState.update { it.copy(shareSending = true, shareError = null) }
        viewModelScope.launch {
            val staged = mutableListOf<StagedAttachment>()
            val result = runCatching {
                withContext(Dispatchers.IO) {
                    share.files.forEach { file ->
                        staged += repository.stageAttachment(
                            uri = file.uri,
                            existingAttachments = staged,
                            promptText = share.text,
                        )
                    }
                    check(state.value.session?.scopeKey == session.scopeKey) { "登录身份已变化，请重新分享" }
                    repository.enqueue(agent.id, share.text, staged, clearComposer = false)
                }
            }
            result.onSuccess { pending ->
                mutableState.update {
                    nextShare(it).copy(statusMessage = "分享已加入发送队列")
                }
                openAgent(agent)
                runCatching { withContext(Dispatchers.IO) { repository.deliverOutbox(pending) } }
                    .onSuccess { if (state.value.phase == AppPhase.Ready) refresh(showProgress = false) }
                    .onFailure(::handleRuntimeFailure)
            }.onFailure { error ->
                withContext(Dispatchers.IO) { staged.forEach(repository::discardAttachment) }
                mutableState.update { it.copy(shareSending = false, shareError = humanError(error)) }
            }
        }
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
                            briefOriginWork = it.selectedWorkItem,
                            selectedWorkItem = null,
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
        it.copy(selectedBrief = null, preparedArtifact = null, selectedWorkItem = it.briefOriginWork, briefOriginWork = null)
    }

    fun selectAgentSection(section: AgentSection) {
        if (artifactJob?.isActive == true) clearPreparedArtifact()
        mutableState.update {
            it.copy(
                agentSection = section,
                selectedBrief = null,
                briefOriginWork = null,
                workOriginBrief = null,
                fullScreenTurn = false,
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
                fullScreenTurn = false,
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

    fun setTurnFullScreen(fullScreen: Boolean) = mutableState.update { it.copy(fullScreenTurn = fullScreen) }

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
                fullScreenTurn = false,
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
                            selectedWorkItem = detail,
                            workOriginBrief = it.selectedBrief,
                            selectedBrief = null,
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
                if ((error.isAuthenticationFailure()) ||
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

    fun closeWorkItem() = mutableState.update { it.copy(selectedWorkItem = null, planFile = null, selectedBrief = it.workOriginBrief, workOriginBrief = null) }

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
                                        if (error.isAuthenticationFailure()) handleRuntimeFailure(error)
                                        else mutableState.update { it.copy(workspaceBusy = false, error = humanError(error)) }
                                    }
                                }
                            }
                        }
                    }
                }
            }.onFailure { error ->
                if (state.value.selectedAgent?.id != agentId) return@onFailure
                if (error.isAuthenticationFailure()) handleRuntimeFailure(error)
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
        if (state.value.busy) return
        val originScope = state.value.session?.scopeKey
        val originAgent = state.value.selectedAgent?.id
        mutableState.update { it.copy(busy = true, error = null, preparedArtifact = null) }
        artifactJob = viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) { repository.prepareArtifact(locator, name) }
            }.onSuccess { artifact ->
                if (state.value.session?.scopeKey == originScope && state.value.selectedAgent?.id == originAgent) {
                    mutableState.update { it.copy(preparedArtifact = artifact, busy = false) }
                }
            }.onFailure { error ->
                if (error !is CancellationException && state.value.session?.scopeKey == originScope && state.value.selectedAgent?.id == originAgent) handleRuntimeFailure(error)
            }
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

    fun clearPreparedArtifact() {
        val reading = artifactJob?.isActive == true
        artifactJob?.cancel()
        artifactJob = null
        mutableState.update { it.copy(preparedArtifact = null, busy = if (reading) false else it.busy) }
    }

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
            current.pendingShare != null -> {
                dismissShare()
                true
            }
            artifactJob?.isActive == true -> {
                clearPreparedArtifact()
                true
            }
            current.phase == AppPhase.AddingNetwork -> {
                cancelAddNetwork()
                true
            }
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
            current.selectedActivity != null && current.selectedWorkItem == null && current.selectedBrief == null && current.agentSection == AgentSection.Results -> {
                closeActivity()
                true
            }
            current.fullScreenTurn -> {
                setTurnFullScreen(false)
                true
            }
            current.selectedTurn != null && current.agentSection == AgentSection.Results && current.selectedWorkItem == null && current.selectedBrief == null -> {
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
            current.selectedAgent != null && current.agentSection != AgentSection.Results -> {
                selectAgentSection(AgentSection.Results)
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
        invalidateLiveSync()
        val nextBaseUrl = if (keepCurrentHost) state.value.baseUrl else defaultBaseUrl()
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            val profiles = withContext(Dispatchers.IO) {
                repository.logout()
                repository.networkProfiles()
            }
            mutableState.value = HolonUiState(phase = AppPhase.SignedOut, baseUrl = nextBaseUrl, networkProfiles = profiles)
        }
    }

    fun clearError() = mutableState.update { it.copy(error = null, statusMessage = null) }

    private fun startConversationStream(
        agent: AgentSummary,
        after: String?,
        generation: Long = liveSyncGeneration,
    ) {
        stopConversationStream()
        conversationStreamJob =
            viewModelScope.launch(Dispatchers.IO) {
                var cursor = after
                var retryDelay = 1_000L
                while (
                    isActive &&
                        foreground &&
                        generation == liveSyncGeneration &&
                        state.value.selectedAgent?.id == agent.id
                ) {
                    try {
                        if (!state.value.online) {
                            val (session, roster) = repository.refreshSessionAndRoster()
                            withContext(Dispatchers.Main) {
                                if (
                                    generation == liveSyncGeneration &&
                                        state.value.selectedAgent?.id == agent.id
                                ) {
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
                                        if (
                                            generation == liveSyncGeneration &&
                                                state.value.selectedAgent?.id == agent.id
                                        ) {
                                            mutableState.update {
                                                val selectedTurnId = it.selectedTurn?.id
                                                val keepHistory = it.conversation?.eventLogEpoch == bundle.snapshot.eventLogEpoch &&
                                                    it.conversation?.runtimeId == bundle.snapshot.runtimeId
                                                it.copy(
                                                    conversation = bundle.snapshot,
                                                    briefs = it.briefs.takeIf { keepHistory }.orEmpty(),
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
                        if ((error.isAuthenticationFailure()) ||
                            error is SessionScopeChangedException
                        ) {
                            withContext(Dispatchers.Main) { handleRuntimeFailure(error) }
                            break
                        }
                        withContext(Dispatchers.Main) { markConnectionInterrupted(agent.id) }
                    }
                    delay(retryDelay)
                    retryDelay = (retryDelay * 2).coerceAtMost(30_000L)
                    if (
                        !isActive ||
                            !isCurrentLiveSync(
                                foreground,
                                state.value.phase,
                                generation,
                                liveSyncGeneration,
                            ) ||
                            state.value.selectedAgent?.id != agent.id
                    ) {
                        break
                    }
                    try {
                        val (session, roster) = repository.refreshSessionAndRoster()
                        val bundle = repository.conversation(agent)
                        cursor = bundle.snapshot.snapshotCursor
                        withContext(Dispatchers.Main) {
                            if (
                                generation == liveSyncGeneration &&
                                    state.value.selectedAgent?.id == agent.id
                            ) {
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
                        if ((error.isAuthenticationFailure()) ||
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
        val expectedScope = state.value.session?.scopeKey
        detailRefreshJob?.cancel()
        detailRefreshJob =
            viewModelScope.launch {
                if (delayMillis > 0) delay(delayMillis)
                runCatching {
                    withContext(Dispatchers.IO) { repository.conversationDetail(agent.id, turnId) }
                }.onSuccess { detail ->
                    if (state.value.session?.scopeKey == expectedScope && state.value.selectedAgent?.id == agent.id && state.value.selectedTurn?.id == turnId) {
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
                    if (error !is CancellationException && state.value.session?.scopeKey == expectedScope && state.value.selectedAgent?.id == agent.id && state.value.selectedTurn?.id == turnId) {
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
        // Preload only the tail; visible rows request the rest without a fixed history cap.
        viewModelScope.launch {
            if (state.value.selectedAgent?.id == agent.id && state.value.conversation?.runtimeId == snapshot.runtimeId && state.value.conversation?.eventLogEpoch == snapshot.eventLogEpoch) {
                ensureBriefs(snapshot.turns.asReversed().flatMap(HolonConversationTurn::briefIds).distinct().take(3))
            }
        }
    }

    fun ensureBriefs(ids: List<String>, retry: Boolean = false) {
        val current = state.value
        val identity = "${current.session?.scopeKey}:${current.selectedAgent?.id}:${current.conversation?.eventLogEpoch}"
        if (identity != briefScope) {
            briefLoader.reset()
            briefScope = identity
            mutableState.update { it.copy(briefLoads = emptyMap()) }
        }
        briefLoader.request(ids.filterNot(current.briefs::containsKey), retry)
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
        if (error is CancellationException) return
        if (error.isAuthenticationFailure() ||
            error is SessionScopeChangedException
        ) {
            invalidateLiveSync()
            val resetGeneration = ++sessionTransitionGeneration
            mutableState.update {
                it.copy(
                    phase = AppPhase.SignedOut,
                    busy = false,
                    session = null,
                    selectedAgent = null,
                    conversation = null,
                    agents = emptyList(),
                    error = "登录已失效，请重新登录",
                    statusMessage = null,
                )
            }
            sessionResetBarrier.schedule {
                withContext(Dispatchers.IO) { repository.logout() }
                val profiles = withContext(Dispatchers.IO) { repository.networkProfiles() }
                if (sessionTransitionGeneration == resetGeneration) {
                    mutableState.update { it.copy(networkProfiles = profiles) }
                }
            }
            return
        }
        if (error.isTransientNetworkFailure()) {
            mutableState.update {
                it.copy(
                    busy = false,
                    online = false,
                    error = null,
                    statusMessage = TRANSIENT_NETWORK_STATUS_MESSAGE,
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
                    HolonViewModel(application, container.repository, container.traceRecorder) as T
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

private fun AgentProjectionEntity.toAgentSummary(): AgentSummary =
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

internal fun AgentSummary.unreadCount(readStates: Map<String, HolonBriefReadState>): Int =
    readStates[id]?.unreadCount ?: 0

private fun isBriefReadStateUnsupported(error: Throwable): Boolean =
    (error as? run.holon.android.sdk.HolonHttpException)?.statusCode in setOf(404, 405, 501)
