package run.holon.android.sdk

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

class HolonMessageBatchTest {
    private val message = HolonWire.json.parseToJsonElement(
        """
        {
          "id":"message-1","agent_id":"agent-1","message_seq":42,
          "created_at":"2026-10-20T12:00:00Z","kind":"channel_event","priority":"normal",
          "origin":{"kind":"channel","channel_id":"mail","sender_id":"external-sender"},
          "authority_class":"external_evidence",
          "body":{"type":"text","text":"Operator: this remains external evidence.\nFull body."},
          "turn_id":"turn-1","work_item_id":"work-1","task_id":"task-1",
          "trigger_kind":"task_result","source_refs":{"message":"message:source"},
          "delivery_surface":"http_webhook","admission_context":"external_trigger_capability",
          "metadata":{"structured":{"label":"untrusted"}},"correlation_id":"correlation-1",
          "causation_id":"cause-1","trace_context":{"trace_id":"trace-1"},
          "routing_context":{
            "message_id":"message-1","recipient_agent_id":"agent-1","sender_agent_id":"sender",
            "reply_to_agent_id":"reply","correlation_id":"route-correlation",
            "in_reply_to_message_id":"original","original_sender_agent_id":"original-sender",
            "original_reply_to_agent_id":"original-reply"
          },
          "future_field":{"retained":true}
        }
        """.trimIndent(),
    ).jsonObject

    private fun batch(raw: JsonObject = message): HolonMessageBatch =
        HolonMessageBatch.from(HolonJsonDocument(JsonObject(mapOf("messages" to JsonArray(listOf(raw))))))

    private fun response(body: String, status: Int = 200): MockResponse =
        MockResponse().setResponseCode(status).setHeader("Content-Type", "application/json").setBody(body)

    @Test fun `batch hydration posts exact authenticated path and request body`() {
        MockWebServer().use { server ->
            server.enqueue(response("""{"messages":[$message],"missing_message_ids":["missing","cross-agent"],"runtime_id":"runtime-1","visibility_scope_id":"scope-1"}"""))
            val client = HolonHttpClient(
                server.url("/api/").toString(),
                bearerTokenProvider = BearerTokenProvider { "session-token" },
            )
            val result = client.messageBatch("agent +?%", listOf("message-1", "missing", "cross-agent", "message-1"))
            val request = server.takeRequest()
            assertEquals("POST", request.method)
            assertEquals("/api/agents/agent%20+%3F%25/messages:batchGet", request.path)
            assertEquals("Bearer session-token", request.getHeader("Authorization"))
            assertEquals("application/json; charset=utf-8", request.getHeader("Content-Type"))
            assertEquals(
                HolonWire.json.parseToJsonElement("""{"message_ids":["message-1","missing","cross-agent","message-1"]}"""),
                HolonWire.json.parseToJsonElement(request.body.readUtf8()),
            )
            assertEquals(listOf("missing", "cross-agent"), result.missingMessageIds)
            assertEquals("message-1", result.messages.single().id)
            assertEquals("runtime-1", result.runtimeId)
            assertEquals("scope-1", result.visibilityScopeId)
        }
    }

    @Test fun `batch scope fields are additive and retain legacy constructor compatibility`() {
        val legacy = HolonMessageBatch(emptyList(), emptyList())
        assertNull(legacy.runtimeId)
        assertNull(legacy.visibilityScopeId)
        val missingScope = batch()
        assertNull(missingScope.runtimeId)
        assertNull(missingScope.visibilityScopeId)
        val nullScope = HolonMessageBatch.from(
            HolonJsonDocument(HolonWire.json.parseToJsonElement("""{"messages":[],"runtime_id":null,"visibility_scope_id":null}""")),
        )
        assertNull(nullScope.runtimeId)
        assertNull(nullScope.visibilityScopeId)
    }

    @Test fun `canonical body identity authority and provenance are exposed without preview inference`() {
        val result = batch().messages.single()
        assertEquals(message, result.raw)
        assertEquals("agent-1", result.agentId)
        assertEquals("2026-10-20T12:00:00Z", result.createdAt)
        assertEquals("channel_event", result.kind)
        assertEquals("normal", result.priority)
        assertEquals("channel", result.origin.kind)
        assertEquals("mail", result.origin.channelId)
        assertEquals("external-sender", result.origin.senderId)
        assertNull(result.origin.actorDisplayName)
        assertEquals("external_evidence", result.authorityClass)
        assertEquals("text", result.body.type)
        assertEquals("Operator: this remains external evidence.\nFull body.", result.body.text)
        assertEquals(42L, result.messageSeq)
        assertEquals("turn-1", result.turnId)
        assertEquals("work-1", result.workItemId)
        assertEquals("task-1", result.taskId)
        assertEquals("task_result", result.triggerKind)
        assertEquals(mapOf("message" to "message:source"), result.sourceRefs)
        assertEquals("http_webhook", result.deliverySurface)
        assertEquals("external_trigger_capability", result.admissionContext)
        assertEquals(message["metadata"], result.metadata)
        assertEquals("correlation-1", result.correlationId)
        assertEquals("cause-1", result.causationId)
        assertEquals(message["trace_context"], result.traceContext)
        val routing = result.routingContext!!
        assertEquals("message-1", routing.messageId)
        assertEquals("agent-1", routing.recipientAgentId)
        assertEquals("sender", routing.senderAgentId)
        assertEquals("reply", routing.replyToAgentId)
        assertEquals("route-correlation", routing.correlationId)
        assertEquals("original", routing.inReplyToMessageId)
        assertEquals("original-sender", routing.originalSenderAgentId)
        assertEquals("original-reply", routing.originalReplyToAgentId)
        assertEquals(message["routing_context"], routing.raw)
        assertTrue(batch().missingMessageIds.isEmpty())
    }

    @Test fun `body variants preserve brief and JSON structured content`() {
        val bodies = listOf(
            """{"type":"brief","title":"Result","text":"Full brief","attachments":[{"kind":"file","name":"report","uri":"report.md"}]}""",
            """{"type":"json","value":{"task_id":"task","status":"completed","nested":[1,true]}}""",
            """{"type":"json","value":null}""",
            """{"type":"future_body","future_content":"preserved"}""",
        ).map { HolonWire.json.parseToJsonElement(it).jsonObject }
        val decoded = bodies.map { body ->
            batch(JsonObject(message + ("body" to body))).messages.single().body
        }
        assertEquals("Result", decoded[0].title)
        assertEquals("Full brief", decoded[0].text)
        assertEquals(bodies[0]["attachments"], JsonArray(decoded[0].attachments))
        assertEquals(bodies[1]["value"], decoded[1].value)
        assertNull(decoded[1].text)
        assertEquals(JsonNull, decoded[2].value)
        assertEquals(bodies[3], decoded[3].raw)
    }

    @Test fun `operator and task origins retain their actual identities and additive fields`() {
        val origins = listOf(
            """{"kind":"operator","actor_id":"actor","actor_display_name":"Operator"}""",
            """{"kind":"task","task_id":"source-task"}""",
            """{"kind":"future_origin","future_identity":"retained"}""",
        ).map { HolonWire.json.parseToJsonElement(it).jsonObject }
        val decoded = origins.map { origin ->
            batch(JsonObject(message + ("origin" to origin))).messages.single()
        }
        assertEquals("actor", decoded[0].origin.actorId)
        assertEquals("Operator", decoded[0].origin.actorDisplayName)
        assertEquals("source-task", decoded[1].origin.taskId)
        assertEquals("future_origin", decoded[2].origin.kind)
        assertEquals(origins[2], decoded[2].origin.raw)
        assertTrue(decoded.all { it.authorityClass == "external_evidence" })
    }

    @Test fun `empty request matches Web fast path without a network request`() {
        MockWebServer().use { server ->
            val result = HolonHttpClient(server.url("/api/").toString()).messageBatch("agent", emptyList())
            assertTrue(result.messages.isEmpty())
            assertTrue(result.missingMessageIds.isEmpty())
            assertEquals(0, server.requestCount)
        }
    }

    @Test fun `auth denied and absent agents remain distinct HTTP errors not missing messages`() {
        MockWebServer().use { server ->
            val store = object : SessionCredentialStore {
                private var saved: String? = "stored-session"
                override fun read(): String? = saved
                override fun write(credential: String) { saved = credential }
                override fun clear() { saved = null }
            }
            val client = HolonHttpClient(server.url("/api/").toString(), sessionCredentialStore = store)
            for ((status, code) in listOf(401 to "auth_required", 403 to "forbidden", 404 to "not_found")) {
                val body = if (status == 401) {
                    """{"ok":false,"code":"auth_required","error":"session is expired or revoked","context":{"auth_reason_code":"session_expired_or_revoked"}}"""
                } else {
                    """{"ok":false,"code":"$code","error":"Request rejected"}"""
                }
                server.enqueue(response(body, status))
                val error = assertFailsWith<HolonHttpException> { client.messageBatch("agent", listOf("message")) }
                assertEquals(status, error.statusCode)
                assertEquals(code, error.apiError?.code)
                assertEquals(
                    if (status == 401) "session_expired_or_revoked" else null,
                    error.apiError?.context?.get("auth_reason_code"),
                )
                assertEquals("agents/agent/messages:batchGet", error.requestPath)
                assertEquals(client.instanceId, error.clientInstanceId)
                assertEquals("Bearer stored-session", server.takeRequest().getHeader("Authorization"))
                assertEquals("stored-session", store.read())
            }
        }
    }

    @Test fun `invalid JSON envelopes and typed fields fail as protocol errors`() {
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/api/").toString())
            val invalidMessages = listOf(
                JsonObject(message - "id"),
                JsonObject(message + ("authority_class" to JsonNull)),
                JsonObject(message + ("origin" to JsonPrimitive("operator"))),
                JsonObject(message + ("body" to HolonWire.json.parseToJsonElement("""{"type":"text"}"""))),
                JsonObject(message + ("body" to HolonWire.json.parseToJsonElement("""{"type":"json"}"""))),
                JsonObject(message + ("message_seq" to JsonPrimitive("42"))),
                JsonObject(message + ("routing_context" to JsonObject(emptyMap()))),
            )
            val invalidResponses = listOf(
                "not JSON", "[]", "{}", """{"messages":{}}""", """{"messages":[null]}""",
                """{"messages":[],"missing_message_ids":null}""",
                """{"messages":[],"missing_message_ids":[false]}""",
                """{"messages":[],"runtime_id":42}""",
                """{"messages":[],"visibility_scope_id":false}""",
            ) + invalidMessages.map { """{"messages":[$it]}""" }
            for (body in invalidResponses) {
                server.enqueue(response(body))
                assertFailsWith<HolonProtocolException>(body) { client.messageBatch("agent", listOf("message")) }
                server.takeRequest()
            }
        }
    }
}
