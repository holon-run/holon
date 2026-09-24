package run.holon.android.app

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Base64
import java.io.File
import java.net.URI
import java.time.Instant
import java.util.UUID
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.BearerTokenProvider
import run.holon.android.sdk.CompatibilityResult
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonBriefAttachment
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonCurrentUser
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonPromptAttachment
import run.holon.android.sdk.HolonProtocolException
import run.holon.android.sdk.HolonRosterSnapshot
import run.holon.android.sdk.HolonServerInfo
import run.holon.android.sdk.HolonSseConnection
import run.holon.android.sdk.HolonWorkItemSnapshot
import run.holon.android.sdk.SessionCredentialStore

internal const val MAX_ATTACHMENT_BYTES = 20L * 1024L * 1024L
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
    val server: HolonServerInfo,
) {
    val scopeKey: String = "$runtimeId:${user.userId}"
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

    suspend fun login(address: String, token: CharArray): Pair<ActiveSession, HolonRosterSnapshot> {
        val baseUrl = normalizeAddress(address)
        var transientToken: String? = token.concatToString()
        token.fill('\u0000')
        val candidate =
            HolonHttpClient(
                baseUrl = baseUrl,
                bearerTokenProvider = BearerTokenProvider { transientToken },
                sessionCredentialStore = sessionStore,
                insecureHttpHosts = debugHttpHosts(),
            )
        return try {
            candidate.exchangeSession(transientToken.orEmpty())
            transientToken = null
            val user = candidate.currentUser()
            val server = requireCompatible(candidate.handshake(REQUIRED_CAPABILITIES))
            val roster = candidate.rosterSnapshot()
            val session = ActiveSession(baseUrl, user, roster.runtimeId, server)
            activate(session, candidate, roster)
            preferences.write(SavedConnection(baseUrl, roster.runtimeId, user.userId))
            session to roster
        } catch (error: Throwable) {
            transientToken = null
            sessionStore.clear()
            throw error
        }
    }

    suspend fun resume(): ResumeResult {
        val saved = preferences.read() ?: return ResumeResult.NoSession
        if (sessionStore.read().isNullOrBlank()) return ResumeResult.NoSession
        val candidate = clientFor(saved.baseUrl)
        return try {
            val user = candidate.currentUser()
            val server = requireCompatible(candidate.handshake(REQUIRED_CAPABILITIES))
            val roster = candidate.rosterSnapshot()
            if (saved.userId != user.userId || saved.runtimeId != roster.runtimeId) {
                clearLocalState()
            }
            val session = ActiveSession(saved.baseUrl, user, roster.runtimeId, server)
            activate(session, candidate, roster)
            preferences.write(SavedConnection(saved.baseUrl, roster.runtimeId, user.userId))
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
        if (roster.runtimeId != current.runtimeId) {
            clearAuthentication()
            throw IllegalStateException("Holon runtime identity changed; sign in again")
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

    suspend fun stageAttachment(uri: Uri, preferredKind: String? = null): StagedAttachment {
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
        metadata?.second?.let { require(it <= MAX_ATTACHMENT_BYTES) { "附件不能超过 20 MB" } }
        val directory = File(context.filesDir, "outbox").apply { mkdirs() }
        val target = File(directory, "${UUID.randomUUID()}-${safeFileName(name)}")
        val copied = resolver.openInputStream(uri)?.use { input ->
            target.outputStream().use { output ->
                val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                var total = 0L
                while (true) {
                    val read = input.read(buffer)
                    if (read < 0) break
                    total += read
                    require(total <= MAX_ATTACHMENT_BYTES) { "附件不能超过 20 MB" }
                    output.write(buffer, 0, read)
                }
                total
            }
        } ?: throw IllegalArgumentException("无法读取所选文件")
        return StagedAttachment(
            kind = preferredKind ?: if (mediaType.startsWith("image/")) "image" else "file",
            name = name,
            mediaType = mediaType,
            localPath = target.absolutePath,
            size = copied,
        )
    }

    suspend fun send(
        agentId: String,
        text: String,
        attachments: List<StagedAttachment>,
        requestId: String = UUID.randomUUID().toString(),
    ): OutboxEntity {
        val session = requireSession()
        val now = System.currentTimeMillis()
        var entry =
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
        dao.putOutbox(entry)
        entry = deliver(entry)
        dao.putOutbox(entry)
        return entry
    }

    suspend fun retryOutbox() {
        val session = requireSession()
        dao.pendingOutbox(session.scopeKey).forEach { pending ->
            dao.putOutbox(deliver(pending))
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
            dao.brief(session.scopeKey, agentId, briefId)?.let { decodeBrief(it.payloadJson) }
                ?: throw error
        }
    }

    suspend fun workItems(agentId: String): List<HolonWorkItemSnapshot> =
        requireClient().workItemSnapshots(agentId, limit = 30)

    suspend fun prepareArtifact(locator: String, preferredName: String): PreparedArtifact {
        val downloaded = requireClient().downloadWorkspaceArtifact(locator)
        val directory = File(context.cacheDir, "shared-artifacts").apply { mkdirs() }
        val name = safeFileName(preferredName.ifBlank { downloaded.fileName })
        val target = File(directory, name)
        target.writeBytes(downloaded.bytes)
        return PreparedArtifact(locator, target.absolutePath, downloaded.mediaType, name)
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
            insecureHttpHosts = debugHttpHosts(),
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
                server = HolonServerInfo("", "cached", true, emptySet()),
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

internal fun normalizeAddress(input: String): String {
    val trimmed = input.trim().trimEnd('/')
    require(trimmed.isNotEmpty()) { "请输入 Holon 地址" }
    val uri = runCatching { URI(trimmed) }.getOrElse { throw IllegalArgumentException("地址格式无效") }
    require(uri.scheme == "https" || (BuildConfig.DEBUG && uri.scheme == "http")) {
        if (BuildConfig.DEBUG) "地址必须使用 HTTPS，调试版允许 HTTP" else "正式版只允许 HTTPS"
    }
    require(uri.host != null && uri.userInfo == null && uri.query == null && uri.fragment == null) {
        "地址必须是完整的 Holon HTTP(S) 地址"
    }
    if (uri.scheme == "http") {
        require(BuildConfig.DEBUG && uri.host in debugHttpHosts()) {
            if (BuildConfig.DEBUG) "调试版 HTTP 仅支持本机或 Android 模拟器" else "正式版只允许 HTTPS"
        }
    }
    if (!BuildConfig.DEBUG && uri.scheme != "https") error("正式版只允许 HTTPS")
    val path = uri.path.trimEnd('/')
    require(path.isEmpty() || path == "/api") { "地址路径只能为空或 /api" }
    return URI(uri.scheme, null, uri.host, uri.port, "/api", null, null).toString().trimEnd('/') + "/"
}

private fun debugHttpHosts(): Set<String> =
    if (BuildConfig.DEBUG) setOf("localhost", "127.0.0.1", "::1", "10.0.2.2") else emptySet()

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

private val SavedConnection.scopeKey: String
    get() = "$runtimeId:$userId"
