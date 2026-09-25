package run.holon.android.sdk

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

public data class HolonCurrentUser(
    public val userId: String,
    public val displayName: String?,
    public val authMethod: String,
)

public data class HolonLatestBrief(
    public val briefId: String,
    public val createdAt: String,
    public val preview: String,
    public val createdEventSeq: Long?,
)

public data class HolonRosterSnapshot(
    public val runtimeId: String,
    public val eventLogEpoch: String,
    public val visibilityScopeId: String,
    public val agents: List<AgentSummary>,
)

public data class HolonPromptAttachment(
    public val kind: String,
    public val name: String?,
    public val mediaType: String,
    public val dataBase64: String,
    public val size: Long,
)

public data class HolonPromptReceipt(
    public val agentId: String,
    public val messageId: String,
    public val disposition: String,
)

public data class HolonPendingInput(
    public val messageId: String,
    public val state: String,
    public val preview: String,
    public val createdAt: String?,
)

public data class HolonTurnInput(
    public val messageId: String,
    public val preview: String,
    public val actorDisplayName: String?,
    public val presentationClass: String?,
)

/** A forward-compatible JSON response from a Holon route. */
public data class HolonJsonDocument(
    public val raw: JsonElement,
) {
    public val objectOrNull: JsonObject?
        get() = raw as? JsonObject
}

public data class HolonEnqueueResult(
    public val ok: Boolean,
    public val agentId: String,
    public val messageId: String?,
    public val raw: JsonObject,
)

public data class HolonTaskSnapshot(
    public val taskId: String,
    public val status: String,
    public val summary: String?,
    public val raw: JsonObject,
) {
    public companion object {
        public fun from(raw: JsonObject): HolonTaskSnapshot =
            HolonTaskSnapshot(
                taskId = raw.string("task_id") ?: raw.string("id") ?: "unknown-task",
                status = raw.string("status") ?: "unknown",
                summary = raw.string("summary"),
                raw = raw,
            )
    }
}

public data class HolonTaskOutputSnapshot(
    public val taskId: String,
    public val status: String,
    public val outputPreview: String?,
    public val resultSummary: String?,
    public val raw: JsonObject,
) {
    public companion object {
        public fun from(raw: JsonObject): HolonTaskOutputSnapshot =
            HolonTaskOutputSnapshot(
                taskId = raw.string("task_id") ?: "unknown-task",
                status = raw.string("status") ?: "unknown",
                outputPreview = raw.string("output_preview"),
                resultSummary = raw.string("result_summary"),
                raw = raw,
            )
    }
}

public data class HolonToolExecutionSnapshot(
    public val toolExecutionId: String,
    public val toolName: String,
    public val status: String,
    public val summary: String?,
    public val artifactCount: Int,
    public val raw: JsonObject,
) {
    public companion object {
        public fun from(raw: JsonObject): HolonToolExecutionSnapshot {
            val output = raw["output"] as? JsonObject
            val result =
                (output?.get("result") as? JsonObject)
                    ?: ((output?.get("envelope") as? JsonObject)?.get("result") as? JsonObject)
                    ?: output
            val artifactCount = (result?.get("artifacts") as? JsonArray)?.size ?: 0
            return HolonToolExecutionSnapshot(
                toolExecutionId = raw.string("id") ?: "unknown-tool-execution",
                toolName = raw.string("tool_name") ?: "unknown-tool",
                status = raw.string("status") ?: "unknown",
                summary = raw.string("summary"),
                artifactCount = artifactCount,
                raw = raw,
            )
        }
    }
}

public data class HolonWorkItemSnapshot(
    public val workItemId: String,
    public val state: String,
    public val objective: String?,
    public val readiness: String?,
    public val schedulingState: String?,
    public val focus: String?,
    public val revision: Long?,
    public val updatedAt: String?,
    public val blockedBy: String?,
    public val recheckAt: String?,
    public val resultBriefId: String?,
    public val resultSummary: String?,
    public val planArtifact: HolonWorkItemPlanArtifact?,
    public val todoList: List<HolonWorkItemTodo>,
    public val workRefs: List<HolonWorkReference>,
    public val raw: JsonObject,
) {
    public companion object {
        public fun from(raw: JsonObject): HolonWorkItemSnapshot =
            HolonWorkItemSnapshot(
                workItemId = raw.string("id") ?: raw.string("work_item_id") ?: "unknown-work-item",
                state = raw.string("state") ?: raw.string("scheduling_state") ?: "unknown",
                objective = raw.string("objective"),
                readiness = raw.string("readiness"),
                schedulingState = raw.string("scheduling_state"),
                focus = raw.string("focus"),
                revision = raw.long("revision"),
                updatedAt = raw.string("updated_at"),
                blockedBy = raw.string("blocked_by"),
                recheckAt = raw.string("recheck_at"),
                resultBriefId = raw.string("result_brief_id"),
                resultSummary = raw.string("result_summary"),
                planArtifact = (raw["plan_artifact"] as? JsonObject)?.let(HolonWorkItemPlanArtifact::from),
                todoList =
                    (raw["todo_list"] as? JsonArray).orEmpty().mapNotNull { element ->
                        val item = element as? JsonObject ?: return@mapNotNull null
                        HolonWorkItemTodo(
                            text = item.string("text") ?: return@mapNotNull null,
                            state = item.string("state") ?: "unknown",
                        )
                    },
                workRefs =
                    (raw["work_refs"] as? JsonArray).orEmpty().mapNotNull { element ->
                        val item = element as? JsonObject ?: return@mapNotNull null
                        HolonWorkReference(
                            kind = item.string("kind") ?: "unknown",
                            ref = item.string("ref") ?: return@mapNotNull null,
                            title = item.string("title"),
                            status = item.string("status"),
                        )
                    },
                raw = raw,
            )
    }
}

public data class HolonWorkItemPlanArtifact(
    public val workspaceId: String?,
    public val relativePath: String?,
    public val preview: String?,
    public val previewComplete: Boolean,
) {
    internal companion object {
        fun from(raw: JsonObject): HolonWorkItemPlanArtifact =
            HolonWorkItemPlanArtifact(
                workspaceId = raw.string("workspace_id"),
                relativePath = raw.string("relative_path"),
                preview = raw.string("preview"),
                previewComplete = raw["preview_complete"]?.jsonPrimitive?.contentOrNull?.toBooleanStrictOrNull() ?: false,
            )
    }
}

public data class HolonWorkItemTodo(
    public val text: String,
    public val state: String,
)

public data class HolonWorkReference(
    public val kind: String,
    public val ref: String,
    public val title: String?,
    public val status: String?,
)

public data class HolonArtifact(
    public val artifactIndex: Int,
    public val size: Long,
    public val content: String,
)

public data class HolonDownloadedArtifact(
    public val bytes: ByteArray,
    public val mediaType: String,
    public val fileName: String,
)

public data class HolonConversationTurn(
    public val id: String,
    public val summary: String,
    public val presentationClass: String?,
    public val inputs: List<HolonTurnInput>,
    public val executionKind: String,
    public val terminalOutcome: String?,
    public val resultKind: String,
    public val attentionKind: String?,
    public val briefIds: List<String>,
    public val startedAt: String?,
    public val completedAt: String?,
    public val settled: Boolean,
    public val raw: JsonObject,
)

public data class HolonConversationSnapshot(
    public val raw: JsonObject,
    public val snapshotCursor: String?,
    public val turns: List<HolonConversationTurn>,
    public val pendingInputs: List<HolonPendingInput>,
    public val runtimeId: String?,
    public val eventLogEpoch: String?,
    public val hasMore: Boolean,
) {
    public companion object {
        public fun from(document: HolonJsonDocument): HolonConversationSnapshot {
            val raw =
                document.objectOrNull
                    ?: throw HolonProtocolException("Holon conversation snapshot is not an object")
            val turns =
                (raw["turns"] as? JsonArray).orEmpty().mapIndexed { index, element ->
                    val turn = element as? JsonObject
                        ?: throw HolonProtocolException("Holon conversation turn $index is not an object")
                    HolonConversationTurn(
                        id = turn.stringValue("turn_id") ?: turn.stringValue("id") ?: "turn-$index",
                        summary = turn.conversationSummary(),
                        presentationClass = turn.stringValue("presentation_class"),
                        inputs =
                            (turn["inputs"] as? JsonArray).orEmpty().mapNotNull { inputElement ->
                                val input = inputElement as? JsonObject ?: return@mapNotNull null
                                HolonTurnInput(
                                    messageId = input.stringValue("message_id") ?: return@mapNotNull null,
                                    preview = input.stringValue("preview").orEmpty().displayTextPreview(),
                                    actorDisplayName = input.stringValue("actor_display_name"),
                                    presentationClass = input.stringValue("presentation_class"),
                                )
                            },
                        executionKind = (turn["execution"] as? JsonObject).stringValue("kind") ?: "unknown",
                        terminalOutcome = (turn["execution"] as? JsonObject).stringValue("outcome"),
                        resultKind = (turn["result"] as? JsonObject).stringValue("kind") ?: "unknown",
                        attentionKind = (turn["attention"] as? JsonObject).stringValue("kind"),
                        briefIds =
                            (turn["brief_ids"] as? JsonArray).orEmpty().mapNotNull {
                                it.jsonPrimitive.contentOrNull
                            },
                        startedAt = turn.stringValue("started_at"),
                        completedAt = turn.stringValue("completed_at"),
                        settled = turn["settled"]?.jsonPrimitive?.contentOrNull?.toBooleanStrictOrNull() ?: false,
                        raw = turn,
                    )
                }
            val pendingInputs =
                (raw["pending_inputs"] as? JsonArray).orEmpty().mapNotNull { inputElement ->
                    val input = inputElement as? JsonObject ?: return@mapNotNull null
                    HolonPendingInput(
                        messageId = input.stringValue("message_id") ?: return@mapNotNull null,
                        state = input.stringValue("state") ?: "unknown",
                        preview = input.stringValue("preview").orEmpty(),
                        createdAt = input.stringValue("created_at"),
                    )
                }
            return HolonConversationSnapshot(
                raw = raw,
                snapshotCursor = raw.stringValue("snapshot_cursor"),
                turns = turns,
                pendingInputs = pendingInputs,
                runtimeId = raw.stringValue("runtime_id"),
                eventLogEpoch = raw.stringValue("event_log_epoch"),
                hasMore = raw["has_more"]?.jsonPrimitive?.contentOrNull?.toBooleanStrictOrNull() ?: false,
            )
        }
    }
}

private fun JsonObject.conversationSummary(): String {
    stringValue("title")?.let { return it }
    stringValue("summary")?.let { return it }

    val execution = this["execution"] as? JsonObject
    val result = this["result"] as? JsonObject
    val hasTerminalEvidence =
        execution.stringValue("kind") == "terminal" ||
            stringValue("completed_at") != null ||
            this["settled"]?.jsonPrimitive?.contentOrNull?.toBooleanStrictOrNull() == true
    return when {
        result.stringValue("kind") == "available" -> "Work result available"
        hasTerminalEvidence ->
            when (execution.stringValue("outcome")) {
                "provider_failed_needs_recovery", "failure" -> "Work failed"
                else -> "Work completed"
            }
        execution.stringValue("kind") == "active" -> "Work in progress"
        else -> stringValue("status") ?: "Work status unavailable"
    }
}

public data class HolonConversationActivity(
    public val id: String,
    public val kind: String,
    public val summary: String,
    public val eventSeq: Long?,
    public val revision: Long?,
    public val raw: JsonObject,
) {
    public val toolExecutionId: String?
        get() = id.takeIf { kind == "tool" && it.startsWith("tool:") }?.removePrefix("tool:")
}

public data class HolonConversationDetail(
    public val activities: List<HolonConversationActivity>,
    public val coverageKind: String,
    public val coverageReason: String?,
    public val hasMore: Boolean,
    public val nextBeforeCursor: String?,
    public val raw: JsonObject,
)

public data class HolonWorkspace(
    public val workspaceId: String,
    public val alias: String?,
    public val label: String,
    public val isActive: Boolean,
    public val executionRootId: String?,
    public val projectionKind: String?,
)

public data class HolonWorkspaceEntry(
    public val name: String,
    public val type: String,
    public val size: Long,
    public val modified: Long?,
    public val mediaType: String?,
)

public data class HolonWorkspaceDirectory(
    public val workspaceId: String,
    public val executionRootId: String?,
    public val path: String,
    public val rootKind: String?,
    public val entries: List<HolonWorkspaceEntry>,
)

public data class HolonBriefAttachment(
    public val kind: String,
    public val name: String,
    public val uri: String?,
    public val value: JsonElement?,
)

public data class HolonBrief(
    public val id: String,
    public val agentId: String,
    public val workItemId: String?,
    public val kind: String,
    public val createdAt: String,
    public val text: String,
    public val attachments: List<HolonBriefAttachment>,
    public val relatedTaskId: String?,
)

public sealed interface HolonConversationStreamEvent {
    public data class BatchBegin(
        public val batchId: String?,
        public val throughSeq: Long?,
    ) : HolonConversationStreamEvent

    public data class Checkpoint(
        public val checkpoint: String,
        public val throughSeq: Long?,
    ) : HolonConversationStreamEvent

    public data class ResetRequired(
        public val reason: String?,
        public val hint: String?,
    ) : HolonConversationStreamEvent

    public data class Mutation(
        public val type: String,
        public val raw: JsonObject,
    ) : HolonConversationStreamEvent

    public data class Unknown(
        public val type: String,
        public val raw: JsonElement?,
    ) : HolonConversationStreamEvent
}

public fun HolonSseEvent.toConversationEvent(): HolonConversationStreamEvent {
    val objectValue = json() as? JsonObject
    val type = objectValue.stringValue("type") ?: event
    return when (type) {
        "batch_begin" ->
            HolonConversationStreamEvent.BatchBegin(
                batchId = objectValue.stringValue("batch_id"),
                throughSeq = objectValue.longValue("through_seq"),
            )
        "checkpoint" ->
            HolonConversationStreamEvent.Checkpoint(
                checkpoint =
                    objectValue.stringValue("checkpoint")
                        ?: id
                        ?: throw HolonProtocolException("Conversation checkpoint is missing its cursor"),
                throughSeq = objectValue.longValue("through_seq"),
            )
        "reset_required" ->
            HolonConversationStreamEvent.ResetRequired(
                reason = objectValue.stringValue("reason"),
                hint = objectValue.stringValue("hint"),
            )
        "operator_upsert", "operator_remove", "turn_summary_upsert",
        "activity_upsert", "detail_invalidated" ->
            HolonConversationStreamEvent.Mutation(type, objectValue ?: JsonObject(emptyMap()))
        else -> HolonConversationStreamEvent.Unknown(type, objectValue ?: json())
    }
}

private fun JsonObject?.stringValue(name: String): String? =
    this?.get(name)?.jsonPrimitive?.contentOrNull

private fun JsonObject?.longValue(name: String): Long? =
    this?.get(name)?.jsonPrimitive?.longOrNull

internal fun String.displayTextPreview(): String {
    if (!trimStart().startsWith('{')) return this
    val objectValue = runCatching {
        HolonWire.json.parseToJsonElement(this) as? JsonObject
    }.getOrNull() ?: return this
    return if (objectValue.stringValue("type") == "text") {
        objectValue.stringValue("text") ?: this
    } else {
        this
    }
}

public data class HolonSseEvent(
    public val event: String = "message",
    public val id: String? = null,
    public val data: String,
) {
    public fun json(): JsonElement? =
        runCatching { HolonWire.json.parseToJsonElement(data) }.getOrNull()

    /**
     * Durable event ordering supplied by the envelope payload.
     *
     * SSE `id` is the reconnect cursor (`Last-Event-ID`); `event_seq` is the
     * only event ordering source and is never inferred from nested payloads.
     */
    public val eventSeq: Long?
        get() =
            json()
                ?.let { element -> (element as? JsonObject)?.get("event_seq")?.jsonPrimitive?.longOrNull }
}

public data class SseReconnectPolicy(
    public val maxAttempts: Int = 5,
    public val initialDelayMillis: Long = 250,
    public val maxDelayMillis: Long = 5_000,
) {
    init {
        require(maxAttempts >= 0) { "maxAttempts must be non-negative" }
        require(initialDelayMillis >= 0) { "initialDelayMillis must be non-negative" }
        require(maxDelayMillis >= initialDelayMillis) {
            "maxDelayMillis must be at least initialDelayMillis"
        }
    }
}

internal fun JsonObject.string(name: String): String? =
    this[name]?.jsonPrimitive?.contentOrNull

internal fun JsonObject.long(name: String): Long? =
    this[name]?.jsonPrimitive?.longOrNull

internal fun JsonObject.int(name: String): Int? =
    long(name)?.takeIf { it in Int.MIN_VALUE..Int.MAX_VALUE }?.toInt()
