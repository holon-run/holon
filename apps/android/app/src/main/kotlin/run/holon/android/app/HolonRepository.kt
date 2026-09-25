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
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.BearerTokenProvider
import run.holon.android.sdk.CompatibilityResult
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonBriefAttachment
import run.holon.android.sdk.HolonConversationDetail
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonPromptAttachment
import run.holon.android.sdk.HolonProtocolException
import run.holon.android.sdk.HolonRosterSnapshot
import run.holon.android.sdk.HolonServerInfo
import run.holon.android.sdk.HolonSseConnection
import run.holon.android.sdk.HolonToolExecutionSnapshot
import run.holon.android.sdk.HolonWorkItemSnapshot
import run.holon.android.sdk.HolonWorkspace
import run.holon.android.sdk.HolonWorkspaceDirectory
import run.holon.android.sdk.SessionCredentialStore

internal val REQUIRED_CAPABILITIES =
    setOf(
        "agents.conversation-read.v1",
        "auth.native-session.v1",
        "control.prompt-idempotency.v1",
        "control.prompt-attachments.v1",
        "brief.attachments.v1",
    )

internal data class ActiveSession(
    val baseUrl: String,
    val user: HolonCurrentUser,
    val runtimeId: String,
    val visibilityScopeId: String,
    val server: HolonServerInfo,
) {
    val scopeKey: String = cacheScopeKey(runtimeId, user.userId, visibilityScopeId)
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
)

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
        val cached: List<ConversationCacheEntity>,
    ) : ResumeResult

    data class Incompatible(
        val baseUrl: String,
        val message: String,
    ) : ResumeResult
}

internal class HolonRepository(
    private val context: Context,
    private val sessionStore: SessionCredentialStore,
    private val preferences: HostPreferences,
    private val dao: HolonDao,
) {
    private val json = Json { ignoreUnknownKeys = true }
    private var active: ActiveSession? = null
    private var client: HolonHttpClient? = null

    suspend fun login(
        address: String,
        token: CharArray,
        allowInsecureHttp: Boolean,
    ): Pair<ActiveSession, HolonRosterSnapshot> {
        val baseUrl = normalizeAddress(address, allowInsecureHttp)
        var transientToken: String? = token.concatToString()
        token.fill('\u0000')
        val candidate =
            HolonHttpClient(
                baseUrl = baseUrl,
                bearerTokenProvider = BearerTokenProvider { transientToken },
                sessionCredentialStore = sessionStore,
                insecureHttpHosts = insecureHttpHosts(baseUrl),
            )
        return try {
            candidate.exchangeSession(transientToken.orEmpty())
            transientToken = null
            val user = candidate.currentUser()
            val server = requireCompatible(candidate.handshake(REQUIRED_CAPABILITIES))
            val roster = candidate.rosterSnapshot()
            val session = ActiveSession(baseUrl, user, roster.runtimeId, roster.visibilityScopeId, server)
            activate(session, candidate, roster)
            preferences.write(
                SavedConnection(baseUrl, roster.runtimeId, user.userId, roster.visibilityScopeId),
            )
            session to roster
        } catch (error: Throwable) {
            transientToken = null
            sessionStore.clear()
            throw error
        }
    }

    suspend fun resume(): ResumeResult {
        val saved = preferences.read()
            ?: run {
                sessionStore.clear()
                return ResumeResult.NoSession
            }
        if (sessionStore.read().isNullOrBlank()) return ResumeResult.NoSession
        val candidate = clientFor(saved.baseUrl)
        return try {
            val user = candidate.currentUser()
            val server = requireCompatible(candidate.handshake(REQUIRED_CAPABILITIES))
            val roster = candidate.rosterSnapshot()
            if (
                saved.userId != user.userId ||
                saved.runtimeId != roster.runtimeId ||
                saved.visibilityScopeId != roster.visibilityScopeId
            ) {
                clearLocalState()
            }
            val session =
                ActiveSession(
                    saved.baseUrl,
                    user,
                    roster.runtimeId,
                    roster.visibilityScopeId,
                    server,
                )
            activate(session, candidate, roster)
            preferences.write(
                SavedConnection(
                    saved.baseUrl,
                    roster.runtimeId,
                    user.userId,
                    roster.visibilityScopeId,
                ),
            )
            retryOutbox()
            ResumeResult.Ready(session, roster)
        } catch (error: HolonHttpException) {
            if (error.statusCode == 401 || error.statusCode == 403) {
                clearAuthentication()
                ResumeResult.NoSession
            } else {
                offlineResult(saved, candidate)
            }
        } catch (error: HolonProtocolException) {
            if (error.isCompatibilityFailure()) {
                ResumeResult.Incompatible(saved.baseUrl, humanError(error))
            } else {
                offlineResult(saved, candidate)
            }
        }
    }

    suspend fun refreshRoster(): HolonRosterSnapshot {
        val roster = requireClient().rosterSnapshot()
        val current = requireSession()
        if (
            roster.runtimeId != current.runtimeId ||
            roster.visibilityScopeId != current.visibilityScopeId
        ) {
            clearAuthentication()
            throw IllegalStateException("Holon runtime or visibility scope changed; sign in again")
        }
        cacheRoster(current.scopeKey, roster)
        return roster
    }

    suspend fun cachedRoster(): List<ConversationCacheEntity> =
        requireSession().let { dao.conversations(it.scopeKey) }

    suspend fun conversation(agent: AgentSummary): ConversationBundle {
        val session = requireSession()
        val snapshot = requireClient().conversationSnapshot(agent.id, limit = 60)
        val existing = dao.conversation(session.scopeKey, agent.id)
        dao.putConversation(
            rosterEntity(session.scopeKey, agent, existing?.updatedAt ?: System.currentTimeMillis()).copy(
                snapshotJson = snapshot.raw.toString(),
                updatedAt = System.currentTimeMillis(),
            ),
        )
        snapshot.snapshotCursor?.let {
            dao.putReadCursor(ReadCursorEntity(session.scopeKey, agent.id, it, System.currentTimeMillis()))
        }
        val authoritativeMessageIds = snapshot.turns.flatMap { turn -> turn.inputs.map { it.messageId } }.toSet()
        dao.outbox(session.scopeKey, agent.id)
            .filter { it.state == "received" && it.messageId in authoritativeMessageIds }
            .forEach {
                deleteOutboxFiles(it)
                dao.deleteOutbox(it.requestId)
            }
        return ConversationBundle(
            snapshot = snapshot,
            outbox = dao.outbox(session.scopeKey, agent.id),
            draft = dao.draft(session.scopeKey, agent.id).orEmpty(),
        )
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
        )
    }

    suspend fun saveDraft(agentId: String, text: String) {
        val scope = requireSession().scopeKey
        dao.putDraft(DraftEntity(scope, agentId, text, System.currentTimeMillis()))
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
        dao.putDraft(DraftEntity(session.scopeKey, agentId, "", now))
        return entry
    }

    suspend fun deliverOutbox(entry: OutboxEntity): OutboxEntity =
        deliver(entry).also { dao.putOutbox(it) }

    suspend fun retryOutbox() {
        val session = requireSession()
        dao.pendingOutbox(session.scopeKey).forEach { pending ->
            deliverOutbox(pending)
        }
    }

    fun openConversationStream(agentId: String, after: String?): HolonSseConnection =
        requireClient().conversationStream(agentId = agentId, after = after, limit = 60, activityLimit = 30)

    suspend fun brief(agentId: String, briefId: String): HolonBrief {
        val session = requireSession()
        return try {
            requireClient().brief(agentId, briefId).also { brief ->
                dao.putBrief(
                    BriefCacheEntity(
                        session.scopeKey,
                        agentId,
                        briefId,
                        encodeBrief(brief),
                        brief.createdAt,
                    ),
                )
            }
        } catch (error: Throwable) {
            if (!error.isTransportFailure()) throw error
            dao.brief(session.scopeKey, agentId, briefId)?.let { decodeBrief(it.payloadJson) }
                ?: throw error
        }
    }

    suspend fun workItems(agentId: String): List<HolonWorkItemSnapshot> =
        requireClient().workItemSnapshots(agentId, limit = 30)

    suspend fun workItem(agentId: String, workItemId: String): HolonWorkItemSnapshot =
        requireClient().workItemSnapshot(agentId, workItemId)

    suspend fun conversationDetail(agentId: String, turnId: String): HolonConversationDetail =
        requireClient().conversationDetail(agentId, turnId, limit = 100)

    suspend fun toolExecution(agentId: String, toolExecutionId: String): HolonToolExecutionSnapshot =
        requireClient().toolExecutionSnapshot(agentId, toolExecutionId)

    suspend fun workspaces(agentId: String): List<HolonWorkspace> =
        requireClient().agentWorkspaces(agentId)

    suspend fun browseWorkspace(
        workspace: HolonWorkspace,
        path: String = "",
    ): HolonWorkspaceDirectory =
        requireClient().browseWorkspaceDirectory(
            workspaceId = workspace.workspaceId,
            path = path,
            executionRootId = workspace.executionRootId,
        )

    suspend fun prepareArtifact(locator: String, preferredName: String): PreparedArtifact {
        val downloaded = requireClient().downloadWorkspaceArtifact(locator)
        return cacheDownloadedArtifact(locator, preferredName, downloaded.bytes, downloaded.mediaType, downloaded.fileName)
    }

    suspend fun prepareWorkspaceFile(
        workspace: HolonWorkspace,
        path: String,
    ): PreparedArtifact {
        val downloaded =
            requireClient().downloadWorkspaceFile(
                workspaceId = workspace.workspaceId,
                path = path,
                executionRootId = workspace.executionRootId,
            )
        val sourceKey = listOf(workspace.workspaceId, workspace.executionRootId.orEmpty(), path).joinToString("|")
        return cacheDownloadedArtifact(sourceKey, downloaded.fileName, downloaded.bytes, downloaded.mediaType, downloaded.fileName)
    }

    private fun cacheDownloadedArtifact(
        locator: String,
        preferredName: String,
        bytes: ByteArray,
        mediaType: String,
        fallbackName: String,
    ): PreparedArtifact {
        val directory = File(context.cacheDir, "shared-artifacts").apply { mkdirs() }
        val name = safeFileName(preferredName.ifBlank { fallbackName })
        val target = File(directory, "${UUID.randomUUID()}-$name")
        target.writeBytes(bytes)
        return PreparedArtifact(locator, target.absolutePath, mediaType, name)
    }

    suspend fun logout() {
        runCatching { client?.logout() }
        clearAuthentication()
    }

    private suspend fun deliver(entry: OutboxEntity): OutboxEntity {
        val sending = entry.copy(state = "sending", error = null, updatedAt = System.currentTimeMillis())
        dao.putOutbox(sending)
        val attachments = json.decodeFromString(ListSerializer(StagedAttachment.serializer()), entry.attachmentsJson)
        return try {
            val receipt =
                requireClient().sendOperatorPrompt(
                    agentId = entry.agentId,
                    text = entry.text,
                    clientRequestId = entry.requestId,
                    attachments = attachments.map { staged ->
                        val file = File(staged.localPath)
                        require(file.isFile) { "待发送附件已不存在：${staged.name}" }
                        HolonPromptAttachment(
                            kind = staged.kind,
                            name = staged.name,
                            mediaType = staged.mediaType,
                            dataBase64 = Base64.encodeToString(file.readBytes(), Base64.NO_WRAP),
                            size = staged.size,
                        )
                    },
                )
            sending.copy(
                state = "received",
                messageId = receipt.messageId,
                updatedAt = System.currentTimeMillis(),
            )
        } catch (error: HolonHttpException) {
            if (error.statusCode == 401 || error.statusCode == 403) throw error
            sending.copy(
                state = if (error.statusCode >= 500) "unknown" else "failed",
                error = humanError(error),
                updatedAt = System.currentTimeMillis(),
            )
        } catch (error: Throwable) {
            sending.copy(
                state = "unknown",
                error = humanError(error),
                updatedAt = System.currentTimeMillis(),
            )
        }
    }

    private suspend fun activate(
        session: ActiveSession,
        newClient: HolonHttpClient,
        roster: HolonRosterSnapshot,
    ) {
        active = session
        client = newClient
        purgeOtherScopes(session.scopeKey)
        cacheRoster(session.scopeKey, roster)
    }

    private suspend fun cacheRoster(scopeKey: String, roster: HolonRosterSnapshot) {
        val old = dao.conversations(scopeKey).associateBy { it.agentId }
        dao.putConversations(
            roster.agents.map { agent ->
                val previous = old[agent.id]
                rosterEntity(
                    scopeKey,
                    agent,
                    latestActivityMillis(agent.latestBrief?.createdAt) ?: previous?.updatedAt ?: 0L,
                ).copy(snapshotJson = previous?.snapshotJson)
            },
        )
    }

    private fun rosterEntity(scopeKey: String, agent: AgentSummary, updatedAt: Long) =
        ConversationCacheEntity(
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

    private fun clientFor(baseUrl: String): HolonHttpClient =
        HolonHttpClient(
            baseUrl = baseUrl,
            sessionCredentialStore = sessionStore,
            insecureHttpHosts = insecureHttpHosts(baseUrl),
        )

    private suspend fun offlineResult(
        saved: SavedConnection,
        candidate: HolonHttpClient,
    ): ResumeResult.Offline {
        val session =
            ActiveSession(
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
        active = session
        client = candidate
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

    private suspend fun purgeOtherScopes(scopeKey: String) {
        dao.purgeOtherConversationScopes(scopeKey)
        dao.purgeOtherDraftScopes(scopeKey)
        dao.purgeOtherOutboxScopes(scopeKey)
        dao.purgeOtherBriefScopes(scopeKey)
        dao.purgeOtherCursorScopes(scopeKey)
    }

    private suspend fun clearLocalState() {
        dao.clearConversations()
        dao.clearDrafts()
        dao.clearOutbox()
        dao.clearBriefs()
        dao.clearCursors()
        File(context.filesDir, "outbox").deleteRecursively()
    }

    private suspend fun clearAuthentication() {
        sessionStore.clear()
        preferences.clear()
        clearLocalState()
        active = null
        client = null
    }

    private fun requireSession(): ActiveSession = checkNotNull(active) { "No active Holon session" }
    private fun requireClient(): HolonHttpClient = checkNotNull(client) { "No active Holon client" }

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

    private fun deleteOutboxFiles(entry: OutboxEntity) {
        runCatching {
            json.decodeFromString(ListSerializer(StagedAttachment.serializer()), entry.attachmentsJson)
                .forEach(::discardAttachment)
        }
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

private fun safeFileName(value: String): String =
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

private fun formatBytes(bytes: Long): String =
    if (bytes % (1024 * 1024) == 0L) "${bytes / (1024 * 1024)} MB" else "$bytes 字节"

private const val SAMPLE_REQUEST_ID = "00000000-0000-0000-0000-000000000000"

private val SavedConnection.scopeKey: String
    get() = cacheScopeKey(runtimeId, userId, visibilityScopeId)
