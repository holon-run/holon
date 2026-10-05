package run.holon.android.app

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Base64
import java.io.File
import java.io.IOException
import java.net.URI
import java.time.Instant
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.BearerTokenProvider
import run.holon.android.sdk.CompatibilityResult
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonBriefAttachment
import run.holon.android.sdk.HolonBriefReadState
import run.holon.android.sdk.HolonMarkBriefReadResult
import run.holon.android.sdk.HolonConversationDetail
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonAgentModelState
import run.holon.android.sdk.HolonModelCatalog
import run.holon.android.sdk.HolonFileReference
import run.holon.android.sdk.HolonFileReferenceResult
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonPromptAttachment
import run.holon.android.sdk.HolonProtocolException
import run.holon.android.sdk.HolonRosterSnapshot
import run.holon.android.sdk.HolonServerInfo
import run.holon.android.sdk.HolonSseConnection
import run.holon.android.sdk.SseReconnectPolicy
import run.holon.android.sdk.HolonToolExecutionSnapshot
import run.holon.android.sdk.HolonWorkItemSnapshot
import run.holon.android.sdk.HolonWorkItemPlanArtifact
import run.holon.android.sdk.HolonWorkspace
import run.holon.android.sdk.HolonWorkspaceDirectory
import run.holon.android.sdk.ProfileSessionCredentialStore
import run.holon.android.sdk.SessionCredentialStore
import run.holon.android.sdk.isTransientHttpStatus

internal val REQUIRED_CAPABILITIES =
    setOf(
        "agents.conversation-read.v1",
        "auth.native-session.v1",
        "control.prompt-idempotency.v1",
        "control.prompt-attachments.v1",
        "brief.attachments.v1",
    )

private const val READ_BRIEF_BASELINE_AGENT_ID = "__android_brief_baseline_v1__"

internal data class ActiveSession(
    val networkId: String,
    val baseUrl: String,
    val user: HolonCurrentUser,
    val runtimeId: String,
    val visibilityScopeId: String,
    val server: HolonServerInfo,
) {
    val scopeKey =
        scopeKeyForNetwork(
            networkId,
            baseUrl,
            runtimeId,
            user.userId,
            visibilityScopeId,
        )
}

@Serializable
internal data class StagedAttachment(
    val kind: String,
    val name: String,
    val mediaType: String,
    val localPath: String,
    val size: Long,
)

internal data class ConversationBundle(
    val snapshot: HolonConversationSnapshot,
    val outbox: List<OutboxEntity>,
    val draft: String,
    val attachments: List<StagedAttachment>,
)

internal fun mergeConversationSnapshots(
    cached: HolonConversationSnapshot?,
    incoming: HolonConversationSnapshot,
): HolonConversationSnapshot = run.holon.android.sdk.mergeConversationPage(cached, incoming, maxTurns = MAX_CACHED_TURNS)

internal data class PreparedArtifact(
    val locator: String,
    val localPath: String,
    val mediaType: String,
    val fileName: String,
)

internal sealed interface ResumeResult {
    data object NoSession : ResumeResult

    data class Ready(
        val session: ActiveSession,
        val roster: HolonRosterSnapshot,
    ) : ResumeResult

    data class Offline(
        val session: ActiveSession,
        val cached: List<AgentProjectionEntity>,
    ) : ResumeResult

    data class Incompatible(
        val baseUrl: String,
        val message: String,
    ) : ResumeResult
}

internal class SessionScopeChangedException : IllegalStateException("Holon 主机或登录身份已改变，请重新登录")

internal class HolonRepository(
    private val context: Context,
    private val sessionStore: SessionCredentialStore,
    private val preferences: HostPreferences,
    private val dao: HolonDao,
    private val traceRecorder: TraceRecorder,
    internal val sessions: SessionCoordinator = SessionCoordinator(),
) {
    private val json = Json { ignoreUnknownKeys = true }
    private val active: ActiveSession? get() = sessions.current?.session
    private val client: HolonHttpClient? get() = sessions.current?.client
    private val outboxDelivery = DurableOutbox(OutboxStore(dao::putOutbox), OutboxSender(::sendOutbox))
    private val files = FilesRepository(context, sessions)
    private val work = WorkRepository(sessions)

    suspend fun login(
        address: String,
        token: CharArray,
        allowInsecureHttp: Boolean,
        pairingTicket: String? = null,
        nativeSessionTicket: String? = null,
        nativeVerifier: String? = null,
    ): Pair<ActiveSession, HolonRosterSnapshot> {
        require(pairingTicket == null || nativeSessionTicket == null) {
            "Pairing and OIDC session tickets are mutually exclusive"
        }
        val baseUrl = normalizeAddress(address, allowInsecureHttp)
        val profile =
            preferences.profiles().firstOrNull { it.baseUrl == baseUrl }
                ?: NetworkProfile(
                    networkId = UUID.randomUUID().toString(),
                    displayName = displayNameFor(baseUrl),
                    baseUrl = baseUrl,
                    allowInsecureHttp = allowInsecureHttp,
                )
        val scopedStore = credentialStore(profile.networkId)
        val traceScope = TraceScope.Network(profile.networkId)
        traceRecorder.record(
            traceScope,
            TraceLevel.INFO,
            "session",
            "session.login.started",
            attributes = mapOf("path" to TraceRedactor.path(baseUrl)),
        )
        val previousCredential = scopedStore.read()
        val previousLease = sessions.current
        var transientToken: String? =
            if (pairingTicket == null && nativeSessionTicket == null) token.concatToString() else null
        token.fill('\u0000')
        val candidate =
            HolonHttpClient(
                baseUrl = baseUrl,
                bearerTokenProvider = BearerTokenProvider { transientToken },
                sessionCredentialStore = scopedStore,
                insecureHttpHosts = insecureHttpHosts(baseUrl),
                eventListenerFactory = traceEventListenerFactory(traceRecorder, traceScope),
                sseRetryObserver = traceSseRetryObserver(traceRecorder, traceScope),
            )
        return try {
            if (nativeSessionTicket != null) {
                candidate.exchangeSession(nativeSessionTicket, nativeVerifier)
            } else if (pairingTicket == null) {
                candidate.exchangeSession(transientToken.orEmpty())
            } else {
                candidate.redeemPairingTicket(pairingTicket)
            }
            transientToken = null
            val user = candidate.currentUser()
            val server = requireCompatible(candidate.handshake(REQUIRED_CAPABILITIES))
            val roster = candidate.rosterSnapshot()
            val session =
                ActiveSession(
                    networkId = profile.networkId,
                    baseUrl = baseUrl,
                    user = user,
                    runtimeId = roster.runtimeId,
                    visibilityScopeId = roster.visibilityScopeId,
                    server = server,
                )
            activate(session, candidate, roster)
            preferences.upsertProfile(
                profile.copy(
                    runtimeId = roster.runtimeId,
                    userId = user.userId,
                    visibilityScopeId = roster.visibilityScopeId,
                    lastUsedAt = System.currentTimeMillis(),
                ),
            )
            traceRecorder.record(
                traceScope,
                TraceLevel.INFO,
                "session",
                "session.login.completed",
                attributes = mapOf("runtimeIdPresent" to (session.runtimeId.isNotBlank()).toString()),
            )
            session to roster
        } catch (error: Throwable) {
            transientToken = null
            sessions.restore(previousLease)
            if (previousCredential.isNullOrBlank()) {
                scopedStore.clear()
            } else {
                scopedStore.write(previousCredential)
            }
            traceRecorder.record(
                traceScope,
                TraceLevel.ERROR,
                "error",
                "session.login.failed",
                attributes = mapOf("errorType" to error::class.simpleName.orEmpty()),
            )
            throw error
        }
    }

    suspend fun resume(): ResumeResult {
        val profile = preferences.selectedProfile()
            ?: run {
                sessionStore.clear()
                return ResumeResult.NoSession
            }
        val runtimeId = profile.runtimeId ?: return ResumeResult.NoSession
        val userId = profile.userId ?: return ResumeResult.NoSession
        val visibilityScopeId = profile.visibilityScopeId ?: return ResumeResult.NoSession
        migrateLegacyCredential(profile)
        migrateSavedScope(profile, runtimeId, userId, visibilityScopeId)
        val saved =
            SavedConnection(
                baseUrl = profile.baseUrl,
                runtimeId = runtimeId,
                userId = userId,
                visibilityScopeId = visibilityScopeId,
                networkId = profile.networkId,
                displayName = profile.displayName,
                allowInsecureHttp = profile.allowInsecureHttp,
            )
        val scopedStore = credentialStore(profile.networkId)
        if (scopedStore.read().isNullOrBlank()) return ResumeResult.NoSession
        val candidate = clientFor(saved.baseUrl, profile.networkId)
        return try {
            val user = candidate.currentUser()
            val server = requireCompatible(candidate.handshake(REQUIRED_CAPABILITIES))
            val roster = candidate.rosterSnapshot()
            if (
                saved.userId != user.userId ||
                saved.runtimeId != roster.runtimeId ||
                saved.visibilityScopeId != roster.visibilityScopeId
            ) {
                clearLocalState(saved.scopeKey)
            }
            val session =
                ActiveSession(
                    networkId = saved.networkId,
                    baseUrl = saved.baseUrl,
                    user = user,
                    runtimeId = roster.runtimeId,
                    visibilityScopeId = roster.visibilityScopeId,
                    server = server,
                )
            activate(session, candidate, roster)
            preferences.upsertProfile(
                profile.copy(
                    runtimeId = roster.runtimeId,
                    userId = user.userId,
                    visibilityScopeId = roster.visibilityScopeId,
                    lastUsedAt = System.currentTimeMillis(),
                ),
            )
            ResumeResult.Ready(session, roster)
        } catch (error: HolonHttpException) {
            if (error.isAuthenticationFailure()) {
                clearAuthentication(networkId = saved.networkId, scopeKey = saved.scopeKey)
                ResumeResult.NoSession
            } else if (error.isTransientNetworkFailure()) {
                offlineResult(saved, candidate)
            } else {
                throw error
            }
        } catch (error: HolonProtocolException) {
            ResumeResult.Incompatible(saved.baseUrl, humanError(error))
        } catch (error: IOException) {
            offlineResult(saved, candidate)
        }
    }

    suspend fun refreshSessionAndRoster(): Pair<ActiveSession, HolonRosterSnapshot> {
        val lease = sessions.capture()
        val client = lease.client
        val current = lease.session
        val user = client.currentUser()
        val server = requireCompatible(client.handshake(REQUIRED_CAPABILITIES))
        val roster = client.rosterSnapshot()
        sessions.requireCurrent(lease)
        if (
            user.userId != current.user.userId ||
            roster.runtimeId != current.runtimeId ||
            roster.visibilityScopeId != current.visibilityScopeId
        ) {
            clearAuthentication()
            throw SessionScopeChangedException()
        }
        sessions.activate(current.copy(user = user, server = server), client)
        cacheRoster(requireSession().scopeKey, roster)
        return requireSession() to roster
    }

    suspend fun keepSessionAlive() {
        val current = requireSession()
        val user = readScoped { it.currentUser() }
        if (user.userId != current.user.userId) {
            clearAuthentication()
            throw SessionScopeChangedException()
        }
    }

    suspend fun modelCatalog(refresh: Boolean = false): HolonModelCatalog =
        withContext(Dispatchers.IO) {
            readScoped { it.modelCatalog(refresh) }
        }

    suspend fun setAgentModel(agentId: String, model: String, reasoningEffort: String?): HolonAgentModelState =
        withContext(Dispatchers.IO) {
            readScoped { it.setAgentModel(agentId, model, reasoningEffort) }
        }

    suspend fun clearAgentModel(agentId: String): HolonAgentModelState =
        withContext(Dispatchers.IO) {
            readScoped { it.clearAgentModel(agentId) }
        }

    suspend fun cachedRoster(): List<AgentProjectionEntity> =
        requireSession().let { dao.conversations(it.scopeKey) }

    suspend fun networkProfiles(): List<NetworkProfile> = preferences.profiles()

    suspend fun switchNetwork(networkId: String): ResumeResult {
        if (active?.networkId == networkId) return resume()
        traceRecorder.record(
            TraceScope.Network(networkId),
            TraceLevel.INFO,
            "network",
            "network.switch.started",
        )
        sessions.clear()
        preferences.selectProfile(networkId)
        return resume().also {
            traceRecorder.record(
                TraceScope.Network(networkId),
                TraceLevel.INFO,
                "network",
                "network.switch.completed",
                attributes = mapOf("result" to it::class.simpleName.orEmpty()),
            )
        }
    }

    suspend fun deleteNetwork(networkId: String) {
        preferences.profiles().firstOrNull { it.networkId == networkId }?.savedScopeKey()?.let {
            clearLocalState(it)
        }
        traceRecorder.record(TraceScope.Network(networkId), TraceLevel.INFO, "network", "network.deleted")
        traceRecorder.delete(TraceScope.Network(networkId))
        credentialStore(networkId).clear()
        preferences.removeProfile(networkId)
        if (active?.networkId == networkId) {
            sessions.clear()
        }
    }

    suspend fun conversation(agent: AgentSummary): ConversationBundle {
        val session = requireSession()
        val existing = dao.conversation(session.scopeKey, agent.id)
        val incoming = readScoped { it.conversationSnapshot(agent.id, limit = 60) }
        ensureCurrentScope(session)
        validateConversationScope(incoming)
        val cached =
            existing?.snapshotJson?.let { raw ->
                runCatching {
                    HolonConversationSnapshot.from(
                        run.holon.android.sdk.HolonJsonDocument(json.parseToJsonElement(raw)),
                    )
                }.getOrNull()
            }
        val snapshot = mergeConversationSnapshots(cached, incoming)
        return acceptConversation(agent, snapshot)
    }

    suspend fun acceptConversation(agent: AgentSummary, snapshot: HolonConversationSnapshot): ConversationBundle {
        val session = requireSession()
        validateConversationScope(snapshot)
        val existing = dao.conversation(session.scopeKey, agent.id)
        val now = System.currentTimeMillis()
        dao.putProjectionAndSync(
            projection =
                rosterEntity(session.scopeKey, agent, existing?.updatedAt ?: now).copy(
                snapshotJson = snapshot.raw.toString(),
                updatedAt = now,
                ),
            syncState =
                AgentSyncStateEntity(
                    scopeKey = session.scopeKey,
                    agentId = agent.id,
                    eventCursor = dao.syncState(session.scopeKey, agent.id)?.eventCursor,
                    conversationCursor = snapshot.snapshotCursor,
                    eventLogEpoch = snapshot.eventLogEpoch,
                    updatedAt = now,
                ),
        )
        val authoritativeMessageIds = (snapshot.turns.flatMap { turn -> turn.inputs.map { it.messageId } } + snapshot.pendingInputs.map { it.messageId }).toSet()
        dao.outbox(session.scopeKey, agent.id)
            .filter { it.state == "received" && it.messageId in authoritativeMessageIds }
            .forEach {
                dao.deleteOutbox(it.requestId)
                deleteOutboxFiles(it)
            }
        return ConversationBundle(
            snapshot = snapshot,
            outbox = dao.outbox(session.scopeKey, agent.id),
            draft = dao.draft(session.scopeKey, agent.id).orEmpty(),
            attachments = composerAttachments(session.scopeKey, agent.id),
        )
    }

    suspend fun olderConversation(agentId: String, before: String): HolonConversationSnapshot =
        readScoped { it.conversationSnapshot(agentId, limit = 60, before = before) }.also {
            validateConversationScope(it)
            cacheConversationSnapshot(agentId, it)
        }

    private suspend fun cacheConversationSnapshot(
        agentId: String,
        incoming: HolonConversationSnapshot,
    ) {
        val session = requireSession()
        val existing = dao.conversation(session.scopeKey, agentId) ?: return
        val cached =
            existing.snapshotJson?.let { raw ->
                runCatching {
                    HolonConversationSnapshot.from(
                        run.holon.android.sdk.HolonJsonDocument(json.parseToJsonElement(raw)),
                    )
                }.getOrNull()
            }
        val merged = run.holon.android.sdk.mergeConversationPage(cached, incoming, history = true, maxTurns = MAX_CACHED_TURNS)
        if (merged.raw.toString() != existing.snapshotJson) {
            dao.putConversation(existing.copy(snapshotJson = merged.raw.toString(), updatedAt = System.currentTimeMillis()))
        }
    }

    private suspend fun validateConversationScope(snapshot: HolonConversationSnapshot) {
        validateReadScope(
            snapshot.runtimeId,
            snapshot.raw["visibility_scope_id"]?.jsonPrimitive?.contentOrNull,
        )
    }

    private suspend fun validateReadScope(runtimeId: String?, visibility: String?) {
        val session = requireSession()
        if ((runtimeId != null && runtimeId != session.runtimeId) ||
            (visibility != null && visibility != session.visibilityScopeId)
        ) {
            clearAuthentication()
            throw SessionScopeChangedException()
        }
    }

    suspend fun cachedConversation(agentId: String): ConversationBundle? {
        val session = requireSession()
        val entity = dao.conversation(session.scopeKey, agentId) ?: return null
        val raw = entity.snapshotJson ?: return null
        val snapshot =
            HolonConversationSnapshot.from(
                run.holon.android.sdk.HolonJsonDocument(json.parseToJsonElement(raw)),
            )
        return ConversationBundle(
            snapshot,
            dao.outbox(session.scopeKey, agentId),
            dao.draft(session.scopeKey, agentId).orEmpty(),
            composerAttachments(session.scopeKey, agentId),
        )
    }

    private suspend fun composerAttachments(scopeKey: String, agentId: String): List<StagedAttachment> =
        dao.composerAttachments(scopeKey, agentId)?.let {
            json.decodeFromString(ListSerializer(StagedAttachment.serializer()), it)
        }.orEmpty().filter { File(it.localPath).isFile }

    suspend fun saveDraft(agentId: String, text: String) {
        val scope = requireSession().scopeKey
        dao.putDraft(DraftEntity(scope, agentId, text, System.currentTimeMillis()))
    }

    suspend fun saveComposerAttachments(agentId: String, attachments: List<StagedAttachment>) {
        dao.putComposerAttachments(
            ComposerAttachmentsEntity(
                scopeKey = requireSession().scopeKey,
                agentId = agentId,
                attachmentsJson = json.encodeToString(ListSerializer(StagedAttachment.serializer()), attachments),
                updatedAt = System.currentTimeMillis(),
            ),
        )
    }

    suspend fun readBriefIds(agents: List<AgentSummary>): Map<String, String> {
        val scopeKey = requireSession().scopeKey
        var entries = dao.readCursors(scopeKey)
        if (entries.none { it.agentId == READ_BRIEF_BASELINE_AGENT_ID && it.cursor == "baseline:v1" } && agents.isNotEmpty()) {
            val now = System.currentTimeMillis()
            val alreadyRead = entries.filter { it.cursor.startsWith("brief:") }.mapTo(mutableSetOf(), ReadCursorEntity::agentId)
            agents.forEach { agent ->
                agent.latestBrief?.briefId?.takeIf { agent.id !in alreadyRead }?.let { briefId ->
                    dao.putReadCursor(ReadCursorEntity(scopeKey, agent.id, "brief:$briefId", now))
                }
            }
            dao.putReadCursor(ReadCursorEntity(scopeKey, READ_BRIEF_BASELINE_AGENT_ID, "baseline:v1", now))
            entries = dao.readCursors(scopeKey)
        }
        return entries.mapNotNull { entry ->
            entry.cursor.removePrefix("brief:").takeIf { entry.cursor.startsWith("brief:") && it.isNotBlank() }
                ?.let { entry.agentId to it }
        }.toMap()
    }

    suspend fun briefReadStates(): Map<String, HolonBriefReadState> =
        readScoped { it.briefReadStates() }.onEach { state ->
            validateReadScope(null, state.visibilityScopeId)
        }.associateBy { it.agentId }

    suspend fun markBriefRead(agentId: String, readThroughEventSeq: Long): HolonMarkBriefReadResult =
        readScoped { it.markBriefRead(agentId, readThroughEventSeq) }.also { result ->
            validateReadScope(null, result.state.visibilityScopeId)
        }

    suspend fun markBriefRead(agentId: String, briefId: String) {
        dao.putReadCursor(ReadCursorEntity(requireSession().scopeKey, agentId, "brief:$briefId", System.currentTimeMillis()))
    }

    fun discardAttachment(attachment: StagedAttachment) {
        runCatching {
            val outboxRoot = File(context.filesDir, "outbox").canonicalFile
            val file = File(attachment.localPath).canonicalFile
            if (file.parentFile == outboxRoot) file.delete()
        }
    }

    suspend fun stageAttachment(
        uri: Uri,
        preferredKind: String? = null,
        existingAttachments: List<StagedAttachment> = emptyList(),
        promptText: String = "",
    ): StagedAttachment {
        val resolver = context.contentResolver
        val metadata = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)
            ?.use { cursor ->
                if (!cursor.moveToFirst()) null else {
                    val nameIndex = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    val sizeIndex = cursor.getColumnIndex(OpenableColumns.SIZE)
                    val name = if (nameIndex >= 0) cursor.getString(nameIndex) else null
                    val size = if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) cursor.getLong(sizeIndex) else null
                    name to size
                }
            }
        val mediaType = resolver.getType(uri) ?: "application/octet-stream"
        val name = metadata?.first?.takeIf(String::isNotBlank) ?: "attachment"
        val kind = preferredKind ?: if (mediaType.startsWith("image/")) "image" else "file"
        val maxBytes = attachmentLimit(kind)
        metadata?.second?.let {
            require(it <= maxBytes) { "${attachmentLabel(kind)}不能超过 ${formatBytes(maxBytes)}" }
        }
        val directory = File(context.filesDir, "outbox").apply { mkdirs() }
        val target = File(directory, "${UUID.randomUUID()}-${safeFileName(name)}")
        return try {
            val copied = resolver.openInputStream(uri)?.use { input ->
                target.outputStream().use { output ->
                    val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                    var total = 0L
                    while (true) {
                        val read = input.read(buffer)
                        if (read < 0) break
                        total += read
                        require(total <= maxBytes) {
                            "${attachmentLabel(kind)}不能超过 ${formatBytes(maxBytes)}"
                        }
                        output.write(buffer, 0, read)
                    }
                    total
                }
            } ?: throw IllegalArgumentException("无法读取所选文件")
            val staged =
                StagedAttachment(
                    kind = kind,
                    name = name,
                    mediaType = mediaType,
                    localPath = target.absolutePath,
                    size = copied,
                )
            validatePromptBody(promptText, existingAttachments + staged, SAMPLE_REQUEST_ID)
            staged
        } catch (error: Throwable) {
            target.delete()
            throw error
        }
    }

    suspend fun enqueue(
        agentId: String,
        text: String,
        attachments: List<StagedAttachment>,
        requestId: String = UUID.randomUUID().toString(),
        clearComposer: Boolean = true,
    ): OutboxEntity {
        val session = requireSession()
        validatePromptBody(text, attachments, requestId)
        val now = System.currentTimeMillis()
        val entry =
            OutboxEntity(
                requestId = requestId,
                scopeKey = session.scopeKey,
                agentId = agentId,
                text = text,
                attachmentsJson = json.encodeToString(ListSerializer(StagedAttachment.serializer()), attachments),
                state = "pending",
                messageId = null,
                error = null,
                createdAt = now,
                updatedAt = now,
            )
        // Persist the retryable request before removing any composer state. If the
        // process stops between these writes, recovery keeps both the outbox entry
        // and the old draft instead of losing the user's message.
        dao.putOutbox(entry)
        if (clearComposer) {
            dao.putDraft(DraftEntity(session.scopeKey, agentId, "", now))
            saveComposerAttachments(agentId, emptyList())
        }
        return entry
    }

    suspend fun deliverOutbox(entry: OutboxEntity): OutboxEntity =
        outboxDelivery.deliver(entry)

    suspend fun editFailedOutbox(entry: OutboxEntity): List<StagedAttachment> {
        require(entry.state == "failed") { "只有未发送成功的消息可以编辑" }
        require(entry.scopeKey == requireSession().scopeKey) { "消息不属于当前登录身份" }
        val attachments = json.decodeFromString(ListSerializer(StagedAttachment.serializer()), entry.attachmentsJson)
        require(attachments.all { File(it.localPath).isFile }) { "待发送附件已不存在，请重新选择文件" }
        saveDraft(entry.agentId, entry.text)
        saveComposerAttachments(entry.agentId, attachments)
        dao.deleteOutbox(entry.requestId)
        return attachments
    }

    suspend fun removeFailedOutbox(entry: OutboxEntity) {
        require(entry.state == "failed") { "只有未发送成功的消息可以移除" }
        require(entry.scopeKey == requireSession().scopeKey) { "消息不属于当前登录身份" }
        dao.deleteOutbox(entry.requestId)
        deleteOutboxFiles(entry)
    }

    suspend fun retryOutbox() {
        val session = requireSession()
        dao.pendingOutbox(session.scopeKey).forEach { pending ->
            deliverOutbox(pending)
        }
    }

    suspend fun outbox(agentId: String): List<OutboxEntity> =
        dao.outbox(requireSession().scopeKey, agentId)

    fun openConversationStream(agentId: String, after: String?): HolonSseConnection =
        requireClient().conversationStream(agentId = agentId, after = after, limit = 60, activityLimit = 30)

    fun reconnectingRosterHints(policy: SseReconnectPolicy = SseReconnectPolicy()): Sequence<String> =
        requireClient().reconnectingRosterHints(policy)

    fun openRosterHints(): RosterHintConnection {
        val stream = requireClient().openSse("events/stream")
        return object : RosterHintConnection {
            override fun hints(): Sequence<String> = stream.events().filter { it.event == "agent_roster_hint" }
                .mapNotNull { (it.json() as? JsonObject)?.get("agent_id")?.jsonPrimitive?.contentOrNull }
            override fun close() = stream.close()
        }
    }


    suspend fun brief(agentId: String, briefId: String): HolonBrief {
        val session = requireSession()
        return try {
            readScoped { it.brief(agentId, briefId) }.also { brief ->
                dao.putBrief(
                    BriefCacheEntity(
                        session.scopeKey,
                        agentId,
                        briefId,
                        encodeBrief(brief),
                        brief.createdAt,
                    ),
                )
                dao.trimBriefs(session.scopeKey, MAX_CACHED_BRIEFS)
            }
        } catch (error: Throwable) {
            if (!error.isTransportFailure()) throw error
            dao.brief(session.scopeKey, agentId, briefId)?.let { decodeBrief(it.payloadJson) }
                ?: throw error
        }
    }

    suspend fun workItems(agentId: String, limit: Int = 30): List<HolonWorkItemSnapshot> =
        work.items(agentId, limit)

    suspend fun operatorPreview(agentId: String): OperatorPreview? =
        readScoped { it.conversationSnapshot(agentId, limit = 1) }.also { validateConversationScope(it) }.operatorPreview()

    suspend fun tasks(agentId: String): List<run.holon.android.sdk.HolonTaskSnapshot> =
        work.tasks(agentId)

    suspend fun task(agentId: String, taskId: String): run.holon.android.sdk.HolonTaskSnapshot =
        work.task(agentId, taskId)

    suspend fun taskOutput(agentId: String, taskId: String): run.holon.android.sdk.HolonTaskOutputSnapshot =
        work.output(agentId, taskId)

    suspend fun workItem(agentId: String, workItemId: String): HolonWorkItemSnapshot =
        work.item(agentId, workItemId)

    suspend fun conversationDetail(agentId: String, turnId: String, before: String? = null): HolonConversationDetail =
        readScoped { it.conversationDetail(agentId, turnId, limit = 100, before = before) }.also { detail ->
            validateReadScope(
                detail.raw["runtime_id"]?.jsonPrimitive?.contentOrNull,
                detail.raw["visibility_scope_id"]?.jsonPrimitive?.contentOrNull,
            )
        }

    suspend fun toolExecution(agentId: String, toolExecutionId: String): HolonToolExecutionSnapshot =
        work.tool(agentId, toolExecutionId)

    suspend fun abortCurrentRun(agentId: String, runId: String) {
        requireClient().abortCurrentRun(agentId, runId)
    }

    suspend fun workspaces(agentId: String): List<HolonWorkspace> = files.workspaces(agentId)
    suspend fun browseWorkspace(workspace: HolonWorkspace, path: String = ""): HolonWorkspaceDirectory =
        files.browseWorkspace(workspace, path)
    suspend fun resolveFileReference(reference: HolonFileReference): HolonFileReferenceResult = files.resolveFileReference(reference)
    suspend fun prepareArtifact(locator: String, preferredName: String): PreparedArtifact = files.prepareArtifact(locator, preferredName)
    suspend fun prepareWorkspaceFile(workspace: HolonWorkspace, path: String): PreparedArtifact = files.prepareWorkspaceFile(workspace, path)
    suspend fun prepareWorkItemPlan(agentId: String, plan: HolonWorkItemPlanArtifact): PreparedArtifact = files.prepareWorkItemPlan(agentId, plan)
    suspend fun saveArtifactToDevice(artifact: PreparedArtifact, destination: Uri) = files.saveArtifactToDevice(artifact, destination)

    suspend fun logout() {
        active?.networkId?.let {
            traceRecorder.record(TraceScope.Network(it), TraceLevel.INFO, "session", "session.logout")
        }
        runCatching { client?.logout() }
        clearAuthentication(removeProfile = true)
    }

    private fun sendOutbox(entry: OutboxEntity): run.holon.android.sdk.HolonPromptReceipt {
        val lease = sessions.capture()
        require(entry.scopeKey == lease.session.scopeKey) { "消息不属于当前登录身份" }
        val attachments = json.decodeFromString(ListSerializer(StagedAttachment.serializer()), entry.attachmentsJson)
        val payload = attachments.map { staged ->
            val file = File(staged.localPath)
            require(file.isFile) { "待发送附件已不存在：${staged.name}" }
            HolonPromptAttachment(
                kind = staged.kind, name = staged.name, mediaType = staged.mediaType,
                dataBase64 = Base64.encodeToString(file.readBytes(), Base64.NO_WRAP), size = staged.size,
            )
        }
        sessions.requireCurrent(lease)
        return lease.client.sendOperatorPrompt(
            agentId = entry.agentId, text = entry.text, clientRequestId = entry.requestId, attachments = payload,
        )
    }
    private suspend fun activate(
        session: ActiveSession,
        newClient: HolonHttpClient,
        roster: HolonRosterSnapshot,
    ) {
        sessions.activate(session, newClient)
        cacheRoster(session.scopeKey, roster)
    }

    private suspend fun cacheRoster(scopeKey: String, roster: HolonRosterSnapshot) {
        val old = dao.conversations(scopeKey).associateBy { it.agentId }
        val session = requireSession()
        val now = System.currentTimeMillis()
        val existingSync = dao.syncStates(scopeKey).associateBy { it.agentId }
        dao.putRosterAndSync(
            scope =
                RuntimeScopeEntity(
                    scopeId = scopeKey,
                    baseUrl = session.baseUrl,
                    runtimeId = session.runtimeId,
                    userId = session.user.userId,
                    visibilityScopeId = session.visibilityScopeId,
                    createdAt = now,
                    lastSeenAt = now,
                ),
            projections =
                roster.agents.map { agent ->
                    val previous = old[agent.id]
                    rosterEntity(
                        scopeKey,
                        agent,
                        latestActivityMillis(agent.latestBrief?.createdAt) ?: previous?.updatedAt ?: 0L,
                    ).copy(snapshotJson = previous?.snapshotJson)
                },
            syncStates =
                roster.agents.mapNotNull { agent ->
                    existingSync[agent.id]?.copy(updatedAt = now)
                },
        )
    }

    private fun rosterEntity(scopeKey: String, agent: AgentSummary, updatedAt: Long) =
        AgentProjectionEntity(
            scopeKey = scopeKey,
            agentId = agent.id,
            displayName = agent.displayName,
            posture = agent.schedulingPosture,
            waitingReason = agent.waitingReason,
            pending = agent.pending,
            latestBriefId = agent.latestBrief?.briefId,
            latestBriefPreview = agent.latestBrief?.preview,
            latestActivityAt = agent.latestBrief?.createdAt,
            snapshotJson = null,
            updatedAt = updatedAt,
        )

    private fun clientFor(baseUrl: String, networkId: String): HolonHttpClient =
        HolonHttpClient(
            baseUrl = baseUrl,
            sessionCredentialStore = credentialStore(networkId),
            insecureHttpHosts = insecureHttpHosts(baseUrl),
            eventListenerFactory = traceEventListenerFactory(traceRecorder, TraceScope.Network(networkId)),
            sseRetryObserver = traceSseRetryObserver(traceRecorder, TraceScope.Network(networkId)),
        )

    private fun credentialStore(networkId: String): SessionCredentialStore {
        val profileStore = sessionStore as? ProfileSessionCredentialStore ?: return sessionStore
        return object : SessionCredentialStore {
            override fun read(): String? = profileStore.read(networkId)
            override fun write(credential: String) = profileStore.write(networkId, credential)
            override fun clear() = profileStore.clear(networkId)
        }
    }

    private suspend fun offlineResult(
        saved: SavedConnection,
        candidate: HolonHttpClient,
    ): ResumeResult.Offline {
        val session =
            ActiveSession(
                networkId = saved.networkId,
                baseUrl = saved.baseUrl,
                user = HolonCurrentUser(saved.userId, null, "cached"),
                runtimeId = saved.runtimeId,
                visibilityScopeId = saved.visibilityScopeId,
                server =
                    HolonServerInfo(
                        defaultAgentId = "",
                        authMode = "cached",
                        authRequired = true,
                        capabilities = emptySet(),
                    ),
            )
        sessions.activate(session, candidate)
        return ResumeResult.Offline(session, dao.conversations(saved.scopeKey))
    }

    private fun requireCompatible(result: CompatibilityResult): HolonServerInfo =
        when (result) {
            is CompatibilityResult.Compatible -> result.server
            is CompatibilityResult.MissingCapabilities ->
                throw HolonProtocolException("daemon 缺少能力：${result.capabilities.sorted().joinToString()}")
            is CompatibilityResult.UnsupportedProtocol ->
                throw HolonProtocolException(
                    "协议不兼容：daemon ${result.actualName}/${result.actualVersion}，请升级 App 或 daemon",
                )
            CompatibilityResult.RejectedHandshake ->
                throw HolonProtocolException("daemon 拒绝了兼容性握手")
        }


    private suspend fun clearLocalState(scopeKey: String) {
        val removed = dao.attachmentPayloads(scopeKey).flatMap(::attachmentPaths).toSet()
        dao.clearScope(scopeKey)
        val retained = dao.attachmentPayloads().flatMap(::attachmentPaths).toSet()
        ScopedFiles(File(context.filesDir, "outbox")).removeUnreferenced(removed, retained)
        File(context.cacheDir, "shared-artifacts/${scopeFileKey(scopeKey)}").deleteRecursively()
    }

    private fun attachmentPaths(payload: String): List<String> =
        runCatching { json.decodeFromString(ListSerializer(StagedAttachment.serializer()), payload) }
            .getOrDefault(emptyList()).map(StagedAttachment::localPath)

    fun isCurrentFailure(error: Throwable): Boolean =
        sessions.acceptsFailure(error)

    private suspend fun clearAuthentication(
        removeProfile: Boolean = false,
        networkId: String? = active?.networkId,
        scopeKey: String? = active?.scopeKey,
    ) {
        val current = active
        val targetNetworkId = networkId ?: current?.networkId
        if (targetNetworkId != null) {
            credentialStore(targetNetworkId).clear()
            if (removeProfile) preferences.removeProfile(targetNetworkId)
        } else {
            sessionStore.clear()
            if (removeProfile) preferences.clear()
        }
        if (scopeKey != null) clearLocalState(scopeKey)
        if (current?.networkId == targetNetworkId) {
            sessions.clear()
        }
    }

    private fun migrateLegacyCredential(profile: NetworkProfile) {
        if (!shouldMigrateLegacyCredential(profile)) return
        (sessionStore as? LegacySessionCredentialMigrator)?.migrateLegacy(profile.networkId)
    }

    private suspend fun migrateSavedScope(profile: NetworkProfile, runtime: String, user: String, visibility: String) {
        val identity = cacheScopeKey(runtime, user, visibility)
        val previous = if (profile.networkId == legacyNetworkId(profile.baseUrl)) identity else "${profile.networkId}:$identity"
        val next = scopeKeyForNetwork(profile.networkId, profile.baseUrl, runtime, user, visibility)
        val owner = dao.runtimeScope(previous)
        // Old v2 caches have no runtime_scope row; an existing owner must match this host.
        if (owner != null && owner.baseUrl != profile.baseUrl) return
        dao.moveScope(previous, next)
        val oldFiles = File(context.cacheDir, "shared-artifacts/${scopeFileKey(previous)}")
        val newFiles = File(context.cacheDir, "shared-artifacts/${scopeFileKey(next)}")
        if (oldFiles.isDirectory && !newFiles.exists()) oldFiles.renameTo(newFiles)
    }

    private fun requireSession(): ActiveSession = checkNotNull(active) { "No active Holon session" }
    private fun requireClient(): HolonHttpClient = checkNotNull(client) { "No active Holon client" }

    private fun ensureCurrentScope(expected: ActiveSession) {
        if (active?.scopeKey != expected.scopeKey) throw kotlinx.coroutines.CancellationException("Stale session response")
    }

    private fun <T> readScoped(read: (HolonHttpClient) -> T): T {
        return sessions.read(read)
    }

    private fun attachmentLimit(kind: String): Long {
        val limits = requireNotNull(requireSession().server.limits) {
            "daemon 未报告附件大小限制，请升级 daemon"
        }
        return when (kind) {
            "image" -> limits.promptImageAttachmentMaxBytes
            "file" -> limits.promptFileAttachmentMaxBytes
            else -> throw IllegalArgumentException("不支持的附件类型：$kind")
        }
    }

    private fun validatePromptBody(
        text: String,
        attachments: List<StagedAttachment>,
        requestId: String,
    ) {
        val maxBytes = requireNotNull(requireSession().server.limits) {
            "daemon 未报告消息大小限制，请升级 daemon"
        }.promptBodyMaxBytes
        val actualBytes = encodedPromptBodySize(text, attachments, requestId)
        require(actualBytes <= maxBytes) {
            "消息和附件编码后不能超过 ${formatBytes(maxBytes)}"
        }
    }

    private suspend fun deleteOutboxFiles(entry: OutboxEntity) {
        val removed = attachmentPaths(entry.attachmentsJson).toSet()
        val retained = dao.attachmentPayloads().flatMap(::attachmentPaths).toSet()
        ScopedFiles(File(context.filesDir, "outbox")).removeUnreferenced(removed, retained)
    }

    private fun encodeBrief(brief: HolonBrief): String =
        json.encodeToString(
            CachedBrief.serializer(),
            CachedBrief(
                brief.id,
                brief.agentId,
                brief.workItemId,
                brief.kind,
                brief.createdAt,
                brief.text,
                brief.attachments.map { CachedAttachment(it.kind, it.name, it.uri, it.value?.toString()) },
                brief.relatedTaskId,
            ),
        )

    private fun decodeBrief(value: String): HolonBrief {
        val brief = json.decodeFromString(CachedBrief.serializer(), value)
        return HolonBrief(
            brief.id,
            brief.agentId,
            brief.workItemId,
            brief.kind,
            brief.createdAt,
            brief.text,
            brief.attachments.map {
                HolonBriefAttachment(
                    it.kind,
                    it.name,
                    it.uri,
                    it.value?.let(json::parseToJsonElement),
                )
            },
            brief.relatedTaskId,
        )
    }
}

@Serializable
private data class CachedAttachment(
    val kind: String,
    val name: String,
    val uri: String?,
    val value: String?,
)

@Serializable
private data class CachedBrief(
    val id: String,
    val agentId: String,
    val workItemId: String?,
    val kind: String,
    val createdAt: String,
    val text: String,
    val attachments: List<CachedAttachment>,
    val relatedTaskId: String?,
)

internal fun normalizeAddress(
    input: String,
    allowInsecureHttp: Boolean = false,
): String {
    val trimmed = input.trim().trimEnd('/')
    require(trimmed.isNotEmpty()) { "请输入 Holon 地址" }
    val uri = runCatching { URI(trimmed) }.getOrElse { throw IllegalArgumentException("地址格式无效") }
    require(uri.scheme == "https" || uri.scheme == "http") { "地址必须使用 HTTP 或 HTTPS" }
    require(uri.host != null && uri.userInfo == null && uri.query == null && uri.fragment == null) {
        "地址必须是完整的 Holon HTTP(S) 地址"
    }
    if (uri.scheme == "http") {
        require(allowInsecureHttp || (BuildConfig.DEBUG && uri.host in debugHttpHosts())) {
            "HTTP 本身不加密，请确认仅在可信网络或加密隧道中使用"
        }
    }
    val path = uri.path.trimEnd('/')
    require(path.isEmpty() || path == "/api") { "地址路径只能为空或 /api" }
    return URI(uri.scheme, null, uri.host, uri.port, "/api", null, null).toString().trimEnd('/') + "/"
}

private fun debugHttpHosts(): Set<String> =
    if (BuildConfig.DEBUG) setOf("localhost", "127.0.0.1", "::1", "10.0.2.2") else emptySet()

private fun insecureHttpHosts(baseUrl: String): Set<String> =
    URI(baseUrl).let { uri -> if (uri.scheme == "http") setOfNotNull(uri.host) else emptySet() }

private fun HolonProtocolException.isCompatibilityFailure(): Boolean =
    message?.let {
        it.startsWith("daemon 缺少能力") ||
            it.startsWith("协议不兼容") ||
            it.startsWith("daemon 拒绝")
    } == true

internal fun humanError(error: Throwable): String =
    when (error) {
        is HolonHttpException ->
            when (error.statusCode) {
                401, 403 -> "登录已失效，请重新登录"
                404 -> "daemon 不支持此功能，请检查版本"
                409 -> error.apiError?.message ?: "请求标识冲突，请重新发送"
                413 -> "附件或请求过大"
                else -> error.apiError?.message ?: "服务端错误（HTTP ${error.statusCode}）"
            }
        is HolonProtocolException ->
            when {
                error.hasCause<javax.net.ssl.SSLException>() -> "TLS 连接失败，请检查证书和 HTTPS 配置"
                error.hasCause<java.net.UnknownHostException>() -> "找不到 Holon 主机，请检查地址"
                error.hasCause<java.net.ConnectException>() -> "无法连接 Holon 主机，请确认 daemon 已启动"
                else -> error.message ?: "daemon 响应不兼容"
            }
        is IllegalArgumentException -> error.message ?: "输入无效"
        is javax.net.ssl.SSLException -> "TLS 连接失败，请检查证书和 HTTPS 配置"
        is java.net.UnknownHostException -> "找不到 Holon 主机，请检查地址"
        is java.net.ConnectException -> "无法连接 Holon 主机，请确认 daemon 已启动"
        else -> "无法连接 Holon，请检查网络和地址"
    }

/** Pairing starts without any session, so an auth failure means the one-time code. */
internal fun pairingHumanError(error: Throwable): String =
    if (error.isAuthenticationFailure()) "配对码无效或已过期，请在 macOS 菜单重新生成"
    else humanError(error)

internal fun Throwable.isAuthenticationFailure(): Boolean {
    return this is HolonHttpException && run.holon.android.sdk.requiresSessionRenewal(statusCode, apiError?.code)
}

internal fun Throwable.isTransientNetworkFailure(): Boolean =
    when (this) {
        is HolonHttpException -> isTransientHttpStatus(statusCode)
        else -> isTransportFailure()
    }

internal const val TRANSIENT_NETWORK_STATUS_MESSAGE: String =
    "网络暂时不可用，已保留当前内容；恢复后可重试"

internal fun safeFileName(value: String): String =
    value.map { if (it.isLetterOrDigit() || it in ".-_ ") it else '_' }.joinToString("").take(120)

private fun latestActivityMillis(value: String?): Long? =
    value?.let { runCatching { Instant.parse(it).toEpochMilli() }.getOrNull() }

private inline fun <reified T : Throwable> Throwable.hasCause(): Boolean {
    var current: Throwable? = this
    while (current != null) {
        if (current is T) return true
        current = current.cause
    }
    return false
}

private fun Throwable.isTransportFailure(): Boolean {
    if (this is HolonHttpException) return false
    if (this is IOException && this !is HolonProtocolException) return true
    var current = cause
    while (current != null) {
        if (current is IOException && current !is HolonHttpException) return true
        current = current.cause
    }
    return false
}

internal fun cacheScopeKey(runtimeId: String, userId: String, visibilityScopeId: String): String =
    listOf(runtimeId, userId, visibilityScopeId).joinToString("|") { "${it.length}:$it" }

internal fun scopeKeyForNetwork(
    networkId: String,
    baseUrl: String,
    runtimeId: String,
    userId: String,
    visibilityScopeId: String,
): String {
    val scope = cacheScopeKey(runtimeId, userId, visibilityScopeId)
    return "$networkId:${baseUrl.length}:$baseUrl:$scope"
}

internal fun shouldMigrateLegacyCredential(profile: NetworkProfile): Boolean =
    profile.networkId == legacyNetworkId(profile.baseUrl)

internal fun encodedPromptBodySize(
    text: String,
    attachments: List<StagedAttachment>,
    requestId: String,
): Long {
    var bytes = utf8Size("{\"text\":") + jsonStringSize(text)
    bytes += utf8Size(",\"client_request_id\":") + jsonStringSize(requestId)
    bytes += utf8Size(",\"attachments\":[")
    attachments.forEachIndexed { index, attachment ->
        if (index > 0) bytes += 1
        bytes += utf8Size("{\"kind\":") + jsonStringSize(attachment.kind)
        bytes += utf8Size(",\"name\":") + jsonStringSize(attachment.name)
        bytes += utf8Size(",\"media_type\":") + jsonStringSize(attachment.mediaType)
        bytes += utf8Size(",\"data_base64\":\"")
        bytes += ((attachment.size + 2) / 3) * 4
        bytes += utf8Size("\"}")
    }
    return bytes + utf8Size("]}")
}

private fun jsonStringSize(value: String): Long =
    JsonPrimitive(value).toString().toByteArray(Charsets.UTF_8).size.toLong()

private fun utf8Size(value: String): Long = value.toByteArray(Charsets.UTF_8).size.toLong()

private fun attachmentLabel(kind: String): String = if (kind == "image") "图片" else "文件"

private const val SAMPLE_REQUEST_ID = "00000000-0000-0000-0000-000000000000"

internal val SavedConnection.scopeKey: String
    get() = scopeKeyForNetwork(networkId, baseUrl, runtimeId, userId, visibilityScopeId)
