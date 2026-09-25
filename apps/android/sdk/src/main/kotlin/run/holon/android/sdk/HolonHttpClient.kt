package run.holon.android.sdk

import java.io.IOException
import java.net.URI
import java.net.URLDecoder
import java.nio.charset.StandardCharsets
import java.util.concurrent.TimeUnit
import kotlinx.serialization.KSerializer
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import run.holon.client.wire.generated.models.AgentListEntry
import run.holon.client.wire.generated.models.CurrentUserResponse
import run.holon.client.wire.generated.models.ErrorResponse
import run.holon.client.wire.generated.models.HandshakeResponse
import run.holon.client.wire.generated.models.NativeSessionResponse
import run.holon.client.wire.generated.models.SessionExchangeRequest

public fun interface BearerTokenProvider {
    public fun token(): String?
}

public class HolonHttpException(
    public val statusCode: Int,
    public val apiError: HolonApiError?,
) : IOException(
    apiError?.let { "${it.code}: ${it.message}" }
        ?: "Holon request failed with HTTP $statusCode",
)

public class HolonProtocolException(
    message: String,
    cause: Throwable? = null,
) : IOException(message, cause)

public data class SessionCredentials(
    public val credential: String,
    public val userId: String,
    public val expiresAt: String?,
)

/**
 * Blocking read-only client for the first Android SDK transport slice.
 *
 * [baseUrl] is the Holon API base URL, including `/api` for a directly connected daemon.
 *
 * Callers must execute these methods away from the Android main thread.
 */
public class HolonHttpClient internal constructor(
    baseUrl: String,
    private val bearerTokenProvider: BearerTokenProvider,
    private val httpClient: OkHttpClient,
    private val sessionCredentialStore: SessionCredentialStore?,
    insecureHttpHosts: Set<String>,
) {
    public constructor(
        baseUrl: String,
        bearerTokenProvider: BearerTokenProvider = BearerTokenProvider { null },
        sessionCredentialStore: SessionCredentialStore? = null,
        insecureHttpHosts: Set<String> = emptySet(),
    ) : this(
        baseUrl = baseUrl,
        bearerTokenProvider = bearerTokenProvider,
        httpClient = defaultHttpClient(),
        sessionCredentialStore = sessionCredentialStore,
        insecureHttpHosts = insecureHttpHosts,
    )

    private val baseUrl: HttpUrl = normalizeBaseUrl(baseUrl, insecureHttpHosts)
    private val sseHttpClient: OkHttpClient =
        sseHttpClient(httpClient)

    public fun handshake(
        requiredCapabilities: Set<String> = emptySet(),
    ): CompatibilityResult =
        get(
            path = "handshake",
            serializer = HandshakeResponse.serializer(),
        ).checkCompatibility(requiredCapabilities)

    public fun listAgents(): List<AgentSummary> =
        get(
            path = "agents/list",
            serializer = ListSerializer(AgentListEntry.serializer()),
        ).map(AgentListEntry::toAgentSummary)

    public fun currentUser(): HolonCurrentUser {
        val response =
            get(
                path = "auth/session/me",
                serializer = CurrentUserResponse.serializer(),
            )
        return HolonCurrentUser(
            userId = response.userId,
            displayName = response.displayName,
            authMethod = response.authMethod,
        )
    }

    public fun rosterSnapshot(): HolonRosterSnapshot {
        val raw = getJson("agents/snapshot").objectOrNull
            ?: throw HolonProtocolException("Holon roster snapshot is not an object")
        val entries = raw["agents"] as? JsonArray
            ?: throw HolonProtocolException("Holon roster snapshot is missing agents")
        val agents =
            entries.mapIndexed { index, element ->
                val entry = element as? JsonObject
                    ?: throw HolonProtocolException("Holon roster entry $index is not an object")
                val agentElement = entry["agent"]
                    ?: throw HolonProtocolException("Holon roster entry $index is missing agent")
                val wire =
                    runCatching {
                        HolonWire.json.decodeFromJsonElement(AgentListEntry.serializer(), agentElement)
                    }.getOrElse { error ->
                        throw HolonProtocolException("Holon roster entry $index is invalid", error)
                    }
                val brief = (entry["latest_brief"] as? JsonObject)?.let { latest ->
                    HolonLatestBrief(
                        briefId = latest.string("brief_id") ?: "unknown-brief",
                        createdAt = latest.string("created_at").orEmpty(),
                        preview = latest.string("preview").orEmpty(),
                        createdEventSeq = latest.long("created_event_seq"),
                    )
                }
                wire.toAgentSummary().copy(latestBrief = brief)
            }
        return HolonRosterSnapshot(
            runtimeId = raw.string("runtime_id")
                ?: throw HolonProtocolException("Holon roster snapshot is missing runtime_id"),
            eventLogEpoch = raw.string("event_log_epoch")
                ?: throw HolonProtocolException("Holon roster snapshot is missing event_log_epoch"),
            visibilityScopeId = raw.string("visibility_scope_id")
                ?: throw HolonProtocolException("Holon roster snapshot is missing visibility_scope_id"),
            agents = agents,
        )
    }

    /**
     * Exchanges a bootstrap/control credential for a revocable session credential.
     *
     * When [sessionCredentialStore] was supplied to the client, the returned
     * credential is persisted through that interface only after a valid response.
     */
    public fun exchangeSession(credential: String): SessionCredentials {
        require(credential.isNotBlank()) { "Session exchange credential must not be blank" }
        val response =
            post(
                path = "auth/session/exchange/native",
                body = SessionExchangeRequest(credential = credential),
                bodySerializer = SessionExchangeRequest.serializer(),
                responseSerializer = NativeSessionResponse.serializer(),
            )
        val session =
            SessionCredentials(
                credential = response.credential,
                userId = response.userId,
                expiresAt = response.expiresAt,
            )
        sessionCredentialStore?.write(session.credential)
        return session
    }

    /**
     * Revokes the current session and clears the platform-provided credential store.
     */
    public fun logout() {
        try {
            postNoContent(
                path = "auth/session/logout",
                preferStoredSession = true,
            )
        } finally {
            sessionCredentialStore?.clear()
        }
    }

    /** Returns a forward-compatible JSON document from a read route. */
    public fun getJson(
        path: String,
        query: Map<String, String> = emptyMap(),
    ): HolonJsonDocument =
        HolonJsonDocument(
            get(
                path = path,
                query = query,
                serializer = JsonElement.serializer(),
            ),
        )

    /** Sends an open JSON request body to a mutation route. */
    public fun postJson(
        path: String,
        body: JsonObject = JsonObject(emptyMap()),
    ): HolonJsonDocument =
        HolonJsonDocument(
            post(
                path = path,
                body = body,
                bodySerializer = JsonElement.serializer(),
                responseSerializer = JsonElement.serializer(),
            ),
        )

    public fun conversation(
        agentId: String,
        limit: Int? = null,
        before: String? = null,
    ): HolonJsonDocument =
        getJson(
            path = "agents/${agentId.pathSegment()}/conversation",
            query =
                buildMap {
                    limit?.let { put("limit", it.toString()) }
                    before?.let { put("before", it) }
                },
        )

    public fun conversationSnapshot(
        agentId: String,
        limit: Int? = null,
        before: String? = null,
    ): HolonConversationSnapshot =
        HolonConversationSnapshot.from(conversation(agentId, limit, before))

    public fun conversationActivities(
        agentId: String,
        turnId: String,
        limit: Int? = null,
        before: String? = null,
    ): List<HolonConversationActivity> =
        conversationDetail(agentId, turnId, limit, before).activities

    public fun conversationDetail(
        agentId: String,
        turnId: String,
        limit: Int? = null,
        before: String? = null,
    ): HolonConversationDetail {
        val raw =
            getJson(
                path = "agents/${agentId.pathSegment()}/turns/${turnId.pathSegment()}/activities",
                query = buildMap {
                    limit?.let { put("limit", it.toString()) }
                    before?.let { put("before", it) }
                },
            ).objectOrNull ?: throw HolonProtocolException("Holon activity response is not an object")
        val activities =
            (raw["activities"] as? JsonArray).orEmpty().mapIndexedNotNull { index, element ->
                val activity = element as? JsonObject
                    ?: throw HolonProtocolException("Holon activity $index is not an object")
                HolonConversationActivity(
                    id = activity.string("id") ?: return@mapIndexedNotNull null,
                    kind = activity.string("kind") ?: "unknown",
                    summary = activity.string("summary").orEmpty().displayTextPreview(),
                    eventSeq = (activity["key"] as? JsonObject)?.long("event_seq"),
                    revision = activity.long("revision"),
                    raw = activity,
                )
            }
        val coverage = raw["coverage"] as? JsonObject
        return HolonConversationDetail(
            activities = activities,
            coverageKind = coverage?.string("kind") ?: "unknown",
            coverageReason = coverage?.string("reason"),
            hasMore = raw["has_more"]?.jsonPrimitive?.contentOrNull?.toBooleanStrictOrNull() ?: false,
            nextBeforeCursor = raw.string("next_before_cursor"),
            raw = raw,
        )
    }

    public fun brief(agentId: String, briefId: String): HolonBrief {
        val raw = getJson("agents/${agentId.pathSegment()}/briefs/${briefId.pathSegment()}").objectOrNull
            ?: throw HolonProtocolException("Holon brief response is not an object")
        val attachments =
            (raw["attachments"] as? JsonArray).orEmpty().mapNotNull { element ->
                val attachment = element as? JsonObject ?: return@mapNotNull null
                HolonBriefAttachment(
                    kind = attachment.string("kind") ?: "unknown",
                    name = attachment.string("name") ?: "Attachment",
                    uri = attachment.string("uri"),
                    value = attachment["value"],
                )
            }
        return HolonBrief(
            id = raw.string("id") ?: briefId,
            agentId = raw.string("agent_id") ?: agentId,
            workItemId = raw.string("work_item_id"),
            kind = raw.string("kind") ?: "turn_result",
            createdAt = raw.string("created_at").orEmpty(),
            text = raw.string("text").orEmpty(),
            attachments = attachments,
            relatedTaskId = raw.string("related_task_id"),
        )
    }

    public fun agentState(agentId: String): HolonJsonDocument =
        getJson("agents/${agentId.pathSegment()}/state")

    public fun agentWorkspaces(agentId: String): List<HolonWorkspace> {
        val raw = agentState(agentId).objectOrNull
            ?: throw HolonProtocolException("Holon agent state is not an object")
        val workspace = raw["workspace"] as? JsonObject
            ?: throw HolonProtocolException("Holon agent state is missing workspace snapshot")
        return (workspace["workspaces"] as? JsonArray).orEmpty().mapIndexed { index, element ->
            val item = element as? JsonObject
                ?: throw HolonProtocolException("Holon workspace $index is not an object")
            val workspaceId = item.string("workspace_id")
                ?: throw HolonProtocolException("Holon workspace $index is missing workspace_id")
            HolonWorkspace(
                workspaceId = workspaceId,
                alias = item.string("workspace_alias"),
                label = item.string("repo_name") ?: item.string("workspace_alias") ?: workspaceId,
                isActive = item["is_active"]?.jsonPrimitive?.contentOrNull?.toBooleanStrictOrNull() ?: false,
                executionRootId = item.string("execution_root_id"),
                projectionKind = item.string("projection_kind"),
            )
        }
    }

    public fun projectionSnapshot(agentId: String): HolonJsonDocument =
        getJson("agents/${agentId.pathSegment()}/projection-snapshot")

    public fun events(agentId: String): HolonJsonDocument =
        getJson("agents/${agentId.pathSegment()}/events")

    public fun tasks(agentId: String, limit: Int? = null): HolonJsonDocument =
        getJson(
            path = "agents/${agentId.pathSegment()}/tasks",
            query = limit?.let { mapOf("limit" to it.toString()) } ?: emptyMap(),
        )

    public fun taskSnapshots(agentId: String, limit: Int? = null): List<HolonTaskSnapshot> {
        val raw = tasks(agentId, limit).raw
        val values =
            when (raw) {
                is JsonArray -> raw
                is JsonObject -> (raw["tasks"] as? JsonArray).orEmpty()
                else -> throw HolonProtocolException("Holon tasks response is not an array")
            }
        return values.mapIndexed { index, value ->
            (value as? JsonObject)
                ?.let(HolonTaskSnapshot::from)
                ?: throw HolonProtocolException("Holon task $index is not an object")
        }
    }

    public fun taskStatus(agentId: String, taskId: String): HolonJsonDocument =
        getJson("agents/${agentId.pathSegment()}/tasks/${taskId.pathSegment()}")

    public fun taskStatusSnapshot(agentId: String, taskId: String): HolonTaskSnapshot {
        val raw = taskStatus(agentId, taskId).objectOrNull
            ?: throw HolonProtocolException("Holon task status response is not an object")
        return HolonTaskSnapshot.from(raw)
    }

    public fun taskOutput(
        agentId: String,
        taskId: String,
        block: Boolean? = null,
        timeoutMillis: Long? = null,
    ): HolonJsonDocument =
        getJson(
            path = "agents/${agentId.pathSegment()}/tasks/${taskId.pathSegment()}/output",
            query =
                buildMap {
                    block?.let { put("block", it.toString()) }
                    timeoutMillis?.let { put("timeout_ms", it.toString()) }
                },
        )

    public fun taskOutputSnapshot(
        agentId: String,
        taskId: String,
        block: Boolean? = null,
        timeoutMillis: Long? = null,
    ): HolonTaskOutputSnapshot {
        val raw = taskOutput(agentId, taskId, block, timeoutMillis).objectOrNull
            ?: throw HolonProtocolException("Holon task output response is not an object")
        return HolonTaskOutputSnapshot.from((raw["task"] as? JsonObject) ?: raw)
    }

    public fun toolExecution(agentId: String, toolExecutionId: String): HolonJsonDocument =
        getJson(
            "agents/${agentId.pathSegment()}/tool-executions/${toolExecutionId.pathSegment()}",
        )

    public fun toolExecutionSnapshot(
        agentId: String,
        toolExecutionId: String,
    ): HolonToolExecutionSnapshot =
        toolExecution(agentId, toolExecutionId).objectOrNull?.let(HolonToolExecutionSnapshot::from)
            ?: throw HolonProtocolException("Holon tool execution response is not an object")

    public fun artifact(
        agentId: String,
        toolExecutionId: String,
        artifactIndex: Int,
    ): HolonArtifact {
        require(artifactIndex >= 0) { "artifactIndex must be non-negative" }
        val objectValue =
            getJson(
                "agents/${agentId.pathSegment()}/tool-executions/" +
                    "${toolExecutionId.pathSegment()}/artifacts/$artifactIndex",
            ).objectOrNull
                ?: throw HolonProtocolException("Holon returned a non-object artifact response")
        val responseArtifactIndex =
            objectValue.int("artifact_index")
                ?: throw HolonProtocolException("Holon artifact response is missing artifact_index")
        if (responseArtifactIndex != artifactIndex) {
            throw HolonProtocolException(
                "Holon artifact response index $responseArtifactIndex does not match requested index $artifactIndex",
            )
        }
        val responseSize =
            objectValue.long("size")
                ?: throw HolonProtocolException("Holon artifact response is missing size")
        if (responseSize < 0) {
            throw HolonProtocolException("Holon artifact response size must be non-negative")
        }
        val responseContent =
            objectValue.string("content")
                ?: throw HolonProtocolException("Holon artifact response is missing content")
        return HolonArtifact(
            artifactIndex = responseArtifactIndex,
            size = responseSize,
            content = responseContent,
        )
    }

    /**
     * Reads an authorized `workspace://` artifact without exposing or interpreting
     * a host filesystem path. Optional `root` locators are preserved as the
     * workspace API's `execution_root_id` selector.
     */
    public fun downloadWorkspaceArtifact(locator: String): HolonDownloadedArtifact {
        val uri = runCatching { URI(locator) }
            .getOrElse { throw HolonProtocolException("Artifact locator is invalid", it) }
        require(uri.scheme == "workspace") { "Only workspace:// artifact locators are supported" }
        val workspaceId = uri.host?.takeIf(String::isNotBlank)
            ?: throw HolonProtocolException("Artifact locator is missing workspace identity")
        val segments = uri.path.orEmpty().trimStart('/').split('/').filter(String::isNotEmpty)
        require(segments.isNotEmpty() && segments.none { it == "." || it == ".." }) {
            "Artifact locator must contain a safe workspace-relative path"
        }
        val rootId = uri.rawQuery
            ?.split('&')
            ?.mapNotNull { field ->
                val parts = field.split('=', limit = 2)
                if (parts.firstOrNull() == "root") {
                    URLDecoder.decode(parts.getOrElse(1) { "" }, StandardCharsets.UTF_8.name())
                } else {
                    null
                }
            }
            ?.firstOrNull()
            ?.takeIf(String::isNotBlank)
        return downloadWorkspaceFile(workspaceId, segments.joinToString("/"), rootId)
    }

    public fun browseWorkspaceDirectory(
        workspaceId: String,
        path: String = "",
        executionRootId: String? = null,
    ): HolonWorkspaceDirectory {
        val safePath = safeWorkspacePath(path)
        val endpoint = workspaceFilePath(workspaceId, safePath)
        val raw = getJson(
            endpoint,
            executionRootId?.let { mapOf("execution_root_id" to it) } ?: emptyMap(),
        ).objectOrNull ?: throw HolonProtocolException("Holon workspace listing is not an object")
        if (raw.string("type") != "directory") {
            throw HolonProtocolException("Holon workspace path is not a directory")
        }
        val entries = (raw["entries"] as? JsonArray).orEmpty().mapIndexed { index, element ->
            val entry = element as? JsonObject
                ?: throw HolonProtocolException("Holon workspace entry $index is not an object")
            HolonWorkspaceEntry(
                name = entry.string("name")
                    ?: throw HolonProtocolException("Holon workspace entry $index is missing name"),
                type = entry.string("type") ?: "unknown",
                size = entry.long("size") ?: 0,
                modified = entry.long("modified"),
                mediaType = entry.string("mime_type"),
            )
        }
        return HolonWorkspaceDirectory(
            workspaceId = raw.string("workspace_id") ?: workspaceId,
            executionRootId = raw.string("execution_root_id") ?: executionRootId,
            path = raw.string("path") ?: safePath,
            rootKind = raw.string("root_kind"),
            entries = entries,
        )
    }

    public fun downloadWorkspaceFile(
        workspaceId: String,
        path: String,
        executionRootId: String? = null,
    ): HolonDownloadedArtifact {
        val safePath = safeWorkspacePath(path)
        require(safePath.isNotEmpty()) { "Workspace file path must not be empty" }
        val response =
            try {
                httpClient.newCall(
                    authorizedRequest(
                        path = workspaceFilePath(workspaceId, safePath),
                        query = buildMap {
                            put("download", "true")
                            executionRootId?.let { put("execution_root_id", it) }
                        },
                    ).header("Accept", "application/octet-stream").get().build(),
                ).execute()
            } catch (error: IOException) {
                throw HolonProtocolException("Holon workspace file request failed", error)
            }
        response.use {
            val body = it.body
            if (!it.isSuccessful) {
                throw httpException(it.code, body?.string().orEmpty())
            }
            val bytes = body?.bytes() ?: throw HolonProtocolException("Holon returned an empty artifact")
            return HolonDownloadedArtifact(
                bytes = bytes,
                mediaType = it.header("Content-Type")?.substringBefore(';') ?: "application/octet-stream",
                fileName = safePath.substringAfterLast('/'),
            )
        }
    }

    public fun enqueue(agentId: String, body: JsonObject): HolonEnqueueResult {
        val objectValue =
            postJson("agents/${agentId.pathSegment()}/enqueue", body).objectOrNull
                ?: throw HolonProtocolException("Holon returned a non-object enqueue response")
        return HolonEnqueueResult(
            ok = objectValue["ok"]?.toString()?.toBoolean() ?: false,
            agentId = objectValue.string("agent_id") ?: agentId,
            messageId = objectValue.string("message_id"),
            raw = objectValue,
        )
    }

    public fun enqueueText(agentId: String, text: String): HolonEnqueueResult =
        enqueue(
            agentId,
            buildJsonObject {
                put("text", text)
            },
        )

    public fun sendOperatorPrompt(
        agentId: String,
        text: String,
        clientRequestId: String,
        attachments: List<HolonPromptAttachment> = emptyList(),
        workItemId: String? = null,
    ): HolonPromptReceipt {
        require(text.isNotBlank() || attachments.isNotEmpty()) {
            "Operator prompt must contain text or an attachment"
        }
        require(clientRequestId.isNotBlank()) { "clientRequestId must not be blank" }
        val body =
            buildJsonObject {
                put("text", text)
                put("client_request_id", clientRequestId)
                workItemId?.takeIf(String::isNotBlank)?.let { put("work_item_id", it) }
                put(
                    "attachments",
                    JsonArray(
                        attachments.map { attachment ->
                            buildJsonObject {
                                put("kind", attachment.kind)
                                attachment.name?.let { put("name", it) }
                                put("media_type", attachment.mediaType)
                                put("data_base64", attachment.dataBase64)
                            }
                        },
                    ),
                )
            }
        val response = postJson("control/agents/${agentId.pathSegment()}/prompt", body).objectOrNull
            ?: throw HolonProtocolException("Holon prompt response is not an object")
        return HolonPromptReceipt(
            agentId = response.string("agent_id") ?: agentId,
            messageId = response.string("message_id")
                ?: throw HolonProtocolException("Holon prompt response is missing message_id"),
            disposition = response.string("disposition") ?: "accepted",
        )
    }

    public fun workItemSnapshots(agentId: String, limit: Int? = null): List<HolonWorkItemSnapshot> {
        val raw =
            getJson(
                path = "agents/${agentId.pathSegment()}/work-items",
                query = limit?.let { mapOf("limit" to it.toString()) } ?: emptyMap(),
            ).raw
        val values =
            when (raw) {
                is JsonArray -> raw
                is JsonObject -> (raw["items"] as? JsonArray).orEmpty()
                else -> throw HolonProtocolException("Holon work-items response is not an array")
            }
        return values.mapIndexed { index, value ->
            (value as? JsonObject)
                ?.let(HolonWorkItemSnapshot::from)
                ?: throw HolonProtocolException("Holon work item $index is not an object")
        }
    }

    public fun workItemSnapshot(agentId: String, workItemId: String): HolonWorkItemSnapshot {
        val raw = getJson(
            "agents/${agentId.pathSegment()}/work-items/${workItemId.pathSegment()}",
        ).objectOrNull ?: throw HolonProtocolException("Holon work item response is not an object")
        return HolonWorkItemSnapshot.from(raw)
    }

    public fun createCommandTask(agentId: String, body: JsonObject): HolonJsonDocument =
        postJson("control/agents/${agentId.pathSegment()}/tasks", body)

    public fun taskInput(
        agentId: String,
        taskId: String,
        body: JsonObject,
    ): HolonJsonDocument =
        postJson(
            "control/agents/${agentId.pathSegment()}/tasks/${taskId.pathSegment()}/input",
            body,
        )

    public fun stopTask(
        agentId: String,
        taskId: String,
        body: JsonObject = JsonObject(emptyMap()),
    ): HolonJsonDocument =
        postJson(
            "control/agents/${agentId.pathSegment()}/tasks/${taskId.pathSegment()}/stop",
            body,
        )

    public fun abortCurrentRun(
        agentId: String,
        runId: String,
    ): HolonJsonDocument =
        postJson(
            "control/agents/${agentId.pathSegment()}/current-run/abort",
            buildJsonObject {
                put("run_id", runId)
                put("mode", "idle_after_abort")
                put("authority_class", "operator_instruction")
            },
        )

    public fun openSse(
        path: String,
        query: Map<String, String> = emptyMap(),
        lastEventId: String? = null,
        deduplicate: Boolean = true,
    ): HolonSseConnection {
        val request =
            authorizedRequest(path, query = query)
                .header("Accept", "text/event-stream")
                .apply {
                    lastEventId?.takeIf(String::isNotBlank)?.let {
                        header("Last-Event-ID", it)
                    }
                }
                .get()
                .build()
        val call = sseHttpClient.newCall(request)
        val response = call.execute()
        if (!response.isSuccessful) {
            val body = response.body?.string().orEmpty()
            val statusCode = response.code
            response.close()
            throw httpException(statusCode, body)
        }
        val body =
            response.body
                ?: run {
                    response.close()
                    throw HolonProtocolException("Holon returned an empty SSE response")
                }
        return HolonSseConnection(
            body = body,
            deduplicator = if (deduplicate) HolonSseDeduplicator() else null,
            cancelCall = call::cancel,
        )
    }

    public fun conversationStream(
        agentId: String,
        after: String? = null,
        limit: Int? = null,
        activityLimit: Int? = null,
        lastEventId: String? = null,
        deduplicate: Boolean = true,
    ): HolonSseConnection =
        openSse(
            path = "agents/${agentId.pathSegment()}/conversation/stream",
            query =
                buildMap {
                    after?.let { put("after", it) }
                    limit?.let { put("limit", it.toString()) }
                    activityLimit?.let { put("activity_limit", it.toString()) }
                },
            lastEventId = lastEventId,
            deduplicate = deduplicate,
        )

    /**
     * Reconnects after clean disconnects, carrying the last SSE cursor and
     * dropping duplicate frames. The sequence ends after [policy.maxAttempts]
     * consecutive reconnects without a newly emitted event.
     */
    public fun reconnectingConversationStream(
        agentId: String,
        after: String? = null,
        limit: Int? = null,
        activityLimit: Int? = null,
        policy: SseReconnectPolicy = SseReconnectPolicy(),
    ): Sequence<HolonSseEvent> =
        reconnectingSse(
            path = "agents/${agentId.pathSegment()}/conversation/stream",
            query =
                buildMap {
                    after?.let { put("after", it) }
                    limit?.let { put("limit", it.toString()) }
                    activityLimit?.let { put("activity_limit", it.toString()) }
                },
            policy = policy,
        )

    public fun conversationChanges(
        agentId: String,
        after: String? = null,
        limit: Int? = null,
        activityLimit: Int? = null,
        lastEventId: String? = null,
    ): Sequence<HolonConversationStreamEvent> =
        sequence {
            val connection =
                conversationStream(
                    agentId = agentId,
                    after = after,
                    limit = limit,
                    activityLimit = activityLimit,
                    lastEventId = lastEventId,
                )
            try {
                for (event in connection.events()) {
                    yield(event.toConversationEvent())
                }
            } finally {
                connection.close()
            }
        }

    public fun reconnectingConversationChanges(
        agentId: String,
        after: String? = null,
        limit: Int? = null,
        activityLimit: Int? = null,
        policy: SseReconnectPolicy = SseReconnectPolicy(),
    ): Sequence<HolonConversationStreamEvent> =
        reconnectingConversationStream(
            agentId = agentId,
            after = after,
            limit = limit,
            activityLimit = activityLimit,
            policy = policy,
        ).map(HolonSseEvent::toConversationEvent)

    private fun reconnectingSse(
        path: String,
        query: Map<String, String>,
        policy: SseReconnectPolicy,
    ): Sequence<HolonSseEvent> =
        sequence {
            val deduplicator = HolonSseDeduplicator()
            var attempts = 0
            var lastEventId: String? = null
            while (true) {
                var emitted = false
                try {
                    val connection =
                        openSse(
                            path = path,
                            query = query,
                            lastEventId = lastEventId,
                            deduplicate = false,
                        )
                    try {
                        for (event in connection.events()) {
                            lastEventId = event.id ?: lastEventId
                            if (deduplicator.accept(event)) {
                                emitted = true
                                yield(event)
                            }
                        }
                    } finally {
                        connection.close()
                    }
                } catch (error: IOException) {
                    if (!error.isRetryableSseFailure()) {
                        throw error
                    }
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    return@sequence
                }
                if (attempts >= policy.maxAttempts) {
                    return@sequence
                }
                val delayMillis =
                    (policy.initialDelayMillis shl attempts.coerceAtMost(20))
                        .coerceAtMost(policy.maxDelayMillis)
                if (delayMillis > 0) {
                    try {
                        Thread.sleep(delayMillis)
                    } catch (_: InterruptedException) {
                        Thread.currentThread().interrupt()
                        return@sequence
                    }
                }
                attempts++
                if (emitted) {
                    attempts = 0
                }
            }
        }

    private fun <T> get(
        path: String,
        query: Map<String, String> = emptyMap(),
        serializer: KSerializer<T>,
    ): T {
        try {
            val response =
                httpClient
                    .newCall(authorizedRequest(path, query = query).get().build())
                    .execute()
            response.use {
                val body = it.body?.string().orEmpty()
                if (!it.isSuccessful) {
                    throw httpException(it.code, body)
                }

                return try {
                    HolonWire.json.decodeFromString(serializer, body)
                } catch (error: Exception) {
                    throw HolonProtocolException(
                        "Holon returned an invalid response for /$path",
                        error,
                    )
                }
            }
        } catch (error: IOException) {
            if (error is HolonHttpException || error is HolonProtocolException) {
                throw error
            }
            throw HolonProtocolException("Holon request failed", error)
        }
    }

    private fun <Request, Response> post(
        path: String,
        body: Request,
        bodySerializer: KSerializer<Request>,
        responseSerializer: KSerializer<Response>,
    ): Response {
        val requestBody =
            HolonWire.json
                .encodeToString(bodySerializer, body)
                .toRequestBody("application/json".toMediaType())
        try {
            val response =
                httpClient
                    .newCall(authorizedRequest(path).post(requestBody).build())
                    .execute()
            response.use {
                val responseText = it.body?.string().orEmpty()
                if (!it.isSuccessful) {
                    throw httpException(it.code, responseText)
                }
                return try {
                    HolonWire.json.decodeFromString(responseSerializer, responseText)
                } catch (error: Exception) {
                    throw HolonProtocolException(
                        "Holon returned an invalid response for /$path",
                        error,
                    )
                }
            }
        } catch (error: IOException) {
            if (error is HolonHttpException || error is HolonProtocolException) {
                throw error
            }
            throw HolonProtocolException("Holon request failed", error)
        }
    }

    private fun postNoContent(
        path: String,
        preferStoredSession: Boolean = false,
    ) {
        try {
            val response =
                httpClient
                    .newCall(
                        authorizedRequest(path, preferStoredSession = preferStoredSession)
                            .post(ByteArray(0).toRequestBody(null))
                            .build(),
                    )
                    .execute()
            response.use {
                val body = it.body?.string().orEmpty()
                if (!it.isSuccessful) {
                    throw httpException(it.code, body)
                }
            }
        } catch (error: IOException) {
            if (error is HolonHttpException || error is HolonProtocolException) {
                throw error
            }
            throw HolonProtocolException("Holon request failed", error)
        }
    }

    private fun authorizedRequest(
        path: String,
        preferStoredSession: Boolean = false,
        query: Map<String, String> = emptyMap(),
    ): Request.Builder {
        val requestBuilder = Request.Builder().url(endpoint(path, query))
        val storedSessionCredential = sessionCredentialStore?.read()?.takeIf(String::isNotBlank)
        val providerCredential = bearerTokenProvider.token()?.takeIf(String::isNotBlank)
        (if (preferStoredSession) {
            storedSessionCredential ?: providerCredential
        } else {
            providerCredential ?: storedSessionCredential
        })
            ?.let { token ->
                requestBuilder.header("Authorization", "Bearer $token")
            }
        return requestBuilder
    }

    private fun httpException(statusCode: Int, body: String): HolonHttpException {
        val apiError =
            runCatching {
                HolonWire.json.decodeFromString(ErrorResponse.serializer(), body)
            }.getOrNull()?.toHolonApiError()
        if (statusCode == 401) {
            sessionCredentialStore?.clear()
        }
        return HolonHttpException(statusCode, apiError)
    }

    private fun endpoint(
        path: String,
        query: Map<String, String> = emptyMap(),
    ): HttpUrl {
        val resolved =
            checkNotNull(baseUrl.resolve(path)) {
                "Failed to resolve Holon endpoint: $path"
            }
        require(resolved.host == baseUrl.host && resolved.port == baseUrl.port) {
            "Holon endpoint must remain on the configured origin"
        }
        return resolved
            .newBuilder()
            .apply {
                query.forEach { (name, value) ->
                    addQueryParameter(name, value)
                }
            }
            .build()
    }

    private fun workspaceFilePath(workspaceId: String, path: String): String =
        buildString {
            append("workspaces/")
            append(workspaceId.pathSegment())
            append("/files")
            if (path.isNotEmpty()) {
                append('/')
                append(path.split('/').joinToString("/") { encodePathSegment(it) })
            }
        }

    private fun safeWorkspacePath(path: String): String {
        val segments = path.trim('/').split('/').filter(String::isNotEmpty)
        require(segments.none { it == "." || it == ".." }) {
            "Workspace path must not contain dot segments"
        }
        return segments.joinToString("/")
    }

    private fun String.pathSegment(): String {
        require(isNotBlank() && !contains('/')) {
            "Holon path identifiers must be non-blank and must not contain '/'"
        }
        return this
    }

    private fun encodePathSegment(value: String): String =
        HttpUrl.Builder()
            .scheme("https")
            .host("placeholder.invalid")
            .addPathSegment(value)
            .build()
            .encodedPathSegments
            .last()

    public companion object {
        internal fun defaultHttpClient(): OkHttpClient =
            OkHttpClient.Builder()
                .callTimeout(30, TimeUnit.SECONDS)
                .followRedirects(false)
                .followSslRedirects(false)
                .build()

        internal fun defaultSseHttpClient(): OkHttpClient =
            sseHttpClient(defaultHttpClient())

        private fun sseHttpClient(httpClient: OkHttpClient): OkHttpClient =
            httpClient
                .newBuilder()
                .callTimeout(0, TimeUnit.MILLISECONDS)
                .readTimeout(45, TimeUnit.SECONDS)
                .build()

        internal fun normalizeBaseUrl(
            value: String,
            insecureHttpHosts: Set<String> = emptySet(),
        ): HttpUrl {
            val parsed =
                requireNotNull(value.toHttpUrlOrNull()) {
                    "Holon base URL must be an absolute HTTP(S) URL"
                }
            require(parsed.username.isEmpty() && parsed.password.isEmpty()) {
                "Holon base URL must not contain user information"
            }
            require(parsed.query == null && parsed.fragment == null) {
                "Holon base URL must not contain a query or fragment"
            }
            require(
                parsed.scheme == "https" ||
                    isLoopbackHttp(parsed) ||
                    (parsed.scheme == "http" && parsed.host in insecureHttpHosts),
            ) {
                "Holon base URL must use HTTPS except for loopback development"
            }

            val normalizedPath = parsed.encodedPath.trimEnd('/') + "/"
            return parsed.newBuilder().encodedPath(normalizedPath).build()
        }

        private fun isLoopbackHttp(url: HttpUrl): Boolean =
            url.scheme == "http" &&
                (
                    url.host == "localhost" ||
                        url.host == "::1" ||
                        isIpv4Loopback(url.host)
                )

        private fun isIpv4Loopback(host: String): Boolean {
            val octets = host.split('.')
            return octets.size == 4 &&
                octets.first() == "127" &&
                octets.all { octet ->
                    octet.isNotEmpty() &&
                        octet.all(Char::isDigit) &&
                        (octet.length == 1 || octet.first() != '0') &&
                        octet.toIntOrNull() in 0..255
                }
        }
    }
}
