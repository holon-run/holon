package run.holon.android.sdk

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.longOrNull

/**
 * Missing includes cross-agent IDs. The wire has no per-message denied list:
 * denied agent access fails the whole request with [HolonHttpException].
 */
public data class HolonMessageBatch(
    public val messages: List<HolonMessage>,
    public val missingMessageIds: List<String>,
    public val runtimeId: String? = null,
    public val visibilityScopeId: String? = null,
) {
    public companion object {
        public fun from(document: HolonJsonDocument): HolonMessageBatch {
            val raw = document.objectOrNull
                ?: throw HolonProtocolException("Holon message batch is not an object")
            val messages = raw["messages"] as? JsonArray
                ?: throw HolonProtocolException("Holon message batch is missing messages")
            return HolonMessageBatch(
                messages = messages.map { message ->
                    HolonMessage.from(message.messageObject("message"))
                },
                missingMessageIds = (raw["missing_message_ids"]?.let {
                    it as? JsonArray ?: throw HolonProtocolException("Invalid Holon message missing_message_ids")
                }.orEmpty()).map {
                    it.messageString("missing_message_ids entry")
                },
                runtimeId = raw.optionalMessageString("runtime_id"),
                visibilityScopeId = raw.optionalMessageString("visibility_scope_id"),
            )
        }
    }
}

/** Canonical identity and authority, never inferred from a conversation preview. */
public data class HolonMessage(
    public val id: String,
    public val agentId: String,
    public val createdAt: String,
    public val kind: String,
    public val origin: HolonMessageOrigin,
    public val authorityClass: String,
    public val priority: String,
    public val body: HolonMessageBody,
    public val raw: JsonObject,
    public val messageSeq: Long? = null,
    public val turnId: String? = null,
    public val workItemId: String? = null,
    public val taskId: String? = null,
    public val triggerKind: String? = null,
    public val sourceRefs: Map<String, String> = emptyMap(),
    public val deliverySurface: String? = null,
    public val admissionContext: String? = null,
    public val metadata: JsonElement? = null,
    public val correlationId: String? = null,
    public val causationId: String? = null,
    public val routingContext: HolonMessageRoutingContext? = null,
    public val traceContext: JsonObject? = null,
) {
    public companion object {
        internal fun from(raw: JsonObject): HolonMessage = HolonMessage(
            id = raw.requiredMessageString("id"),
            agentId = raw.requiredMessageString("agent_id"),
            createdAt = raw.requiredMessageString("created_at"),
            kind = raw.requiredMessageString("kind"),
            origin = HolonMessageOrigin.from(raw["origin"].messageObject("origin")),
            authorityClass = raw.requiredMessageString("authority_class"),
            priority = raw.requiredMessageString("priority"),
            body = HolonMessageBody.from(raw["body"].messageObject("body")),
            raw = raw,
            messageSeq = raw["message_seq"].messageValue()?.let {
                (it as? JsonPrimitive)?.takeUnless { value -> value.isString }?.longOrNull
                    ?: throw HolonProtocolException("Invalid Holon message message_seq")
            },
            turnId = raw.optionalMessageString("turn_id"),
            workItemId = raw.optionalMessageString("work_item_id"),
            taskId = raw.optionalMessageString("task_id"),
            triggerKind = raw.optionalMessageString("trigger_kind"),
            sourceRefs = raw["source_refs"].messageValue()?.messageObject("source_refs")
                ?.mapValues { (_, value) -> value.messageString("source_refs entry") }.orEmpty(),
            deliverySurface = raw.optionalMessageString("delivery_surface"),
            admissionContext = raw.optionalMessageString("admission_context"),
            metadata = raw["metadata"].messageValue(),
            correlationId = raw.optionalMessageString("correlation_id"),
            causationId = raw.optionalMessageString("causation_id"),
            routingContext = raw["routing_context"].messageValue()?.let {
                HolonMessageRoutingContext.from(it.messageObject("routing_context"))
            },
            traceContext = raw["trace_context"].messageValue()?.messageObject("trace_context"),
        )
    }
}

/** Open kind strings preserve new origin variants without promoting their authority. */
public data class HolonMessageOrigin(
    public val kind: String,
    public val raw: JsonObject,
    public val actorId: String? = null,
    public val actorDisplayName: String? = null,
    public val channelId: String? = null,
    public val senderId: String? = null,
    public val source: String? = null,
    public val eventType: String? = null,
    public val descriptorId: String? = null,
    public val timerId: String? = null,
    public val subsystem: String? = null,
    public val taskId: String? = null,
) {
    public companion object {
        internal fun from(raw: JsonObject): HolonMessageOrigin = HolonMessageOrigin(
            kind = raw.requiredMessageString("kind"),
            raw = raw,
            actorId = raw.optionalMessageString("actor_id"),
            actorDisplayName = raw.optionalMessageString("actor_display_name"),
            channelId = raw.optionalMessageString("channel_id"),
            senderId = raw.optionalMessageString("sender_id"),
            source = raw.optionalMessageString("source"),
            eventType = raw.optionalMessageString("event_type"),
            descriptorId = raw.optionalMessageString("descriptor_id"),
            timerId = raw.optionalMessageString("timer_id"),
            subsystem = raw.optionalMessageString("subsystem"),
            taskId = raw.optionalMessageString("task_id"),
        )
    }
}

public data class HolonMessageBody(
    public val type: String,
    public val raw: JsonObject,
    public val text: String? = null,
    public val title: String? = null,
    public val value: JsonElement? = null,
    public val attachments: List<JsonObject> = emptyList(),
) {
    public companion object {
        internal fun from(raw: JsonObject): HolonMessageBody {
            val type = raw.requiredMessageString("type")
            val text = raw.optionalMessageString("text")
            if (type in listOf("text", "brief") && text == null) {
                throw HolonProtocolException("Holon message $type body is missing text")
            }
            if (type == "json" && !raw.containsKey("value")) {
                throw HolonProtocolException("Holon message JSON body is missing value")
            }
            return HolonMessageBody(
                type = type,
                raw = raw,
                text = text,
                title = raw.optionalMessageString("title"),
                value = raw["value"],
                attachments = raw.messageArray("attachments").map { it.messageObject("attachment") },
            )
        }
    }
}

public data class HolonMessageRoutingContext(
    public val messageId: String,
    public val recipientAgentId: String,
    public val raw: JsonObject,
    public val senderAgentId: String? = null,
    public val replyToAgentId: String? = null,
    public val correlationId: String? = null,
    public val inReplyToMessageId: String? = null,
    public val originalSenderAgentId: String? = null,
    public val originalReplyToAgentId: String? = null,
) {
    public companion object {
        internal fun from(raw: JsonObject): HolonMessageRoutingContext = HolonMessageRoutingContext(
            messageId = raw.requiredMessageString("message_id"),
            recipientAgentId = raw.requiredMessageString("recipient_agent_id"),
            raw = raw,
            senderAgentId = raw.optionalMessageString("sender_agent_id"),
            replyToAgentId = raw.optionalMessageString("reply_to_agent_id"),
            correlationId = raw.optionalMessageString("correlation_id"),
            inReplyToMessageId = raw.optionalMessageString("in_reply_to_message_id"),
            originalSenderAgentId = raw.optionalMessageString("original_sender_agent_id"),
            originalReplyToAgentId = raw.optionalMessageString("original_reply_to_agent_id"),
        )
    }
}

private fun JsonElement?.messageValue(): JsonElement? = takeUnless { it == JsonNull }

private fun JsonElement?.messageObject(field: String): JsonObject =
    this as? JsonObject ?: throw HolonProtocolException("Invalid Holon message $field")

private fun JsonElement?.messageString(field: String): String =
    (this as? JsonPrimitive)?.takeIf { it.isString }?.content
        ?: throw HolonProtocolException("Invalid Holon message $field")

private fun JsonObject.requiredMessageString(field: String): String =
    this[field].messageString(field)

private fun JsonObject.optionalMessageString(field: String): String? =
    this[field].messageValue()?.messageString(field)

private fun JsonObject.messageArray(field: String): List<JsonElement> =
    this[field].messageValue()?.let {
        it as? JsonArray ?: throw HolonProtocolException("Invalid Holon message $field")
    }.orEmpty()
