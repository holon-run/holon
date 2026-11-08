package run.holon.android.sdk

import java.net.HttpURLConnection
import java.util.concurrent.TimeUnit
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.OkHttpClient
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.SocketPolicy
import okio.Buffer
import org.junit.Test
import kotlin.test.assertEquals
import kotlin.test.assertContentEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

class HolonHttpClientTest {
    @Test
    fun `avatar reads PNG with pinned defaults and bearer authentication`() {
        MockWebServer().use { server ->
            val bytes = byteArrayOf(0x89.toByte(), 0x50, 0x4e, 0x47)
            server.enqueue(pngResponse(bytes).setHeader("Content-Type", "image/png; charset=binary"))
            val client = HolonHttpClient(
                server.url("/api/").toString(),
                bearerTokenProvider = BearerTokenProvider { "avatar-token" },
            )

            assertContentEquals(bytes, client.readAgentAvatar("agent_1"))

            val request = server.takeRequest()
            assertEquals("GET", request.method)
            assertEquals(
                "/api/agents/agent_1/avatar?version=light-mech-1-vivid-2&expression=neutral&format=png&size=128",
                request.path,
            )
            assertEquals("Bearer avatar-token", request.getHeader("Authorization"))
            assertEquals("image/png", request.getHeader("Accept"))
            assertEquals(1, server.requestCount)
        }
    }

    @Test
    fun `avatar expressions and supported sizes use their explicit wire values`() {
        assertEquals(
            listOf("neutral", "focused", "questioning", "waiting", "sleeping"),
            HolonAvatarExpression.entries.map { it.wireValue },
        )
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/").toString())
            HolonAvatarExpression.entries.zip(listOf(32, 64, 128, 256, 512)).forEach { (expression, size) ->
                server.enqueue(pngResponse())

                client.readAgentAvatar("agent_1", expression, size)

                val url = server.takeRequest().requestUrl
                assertEquals(expression.wireValue, url?.queryParameter("expression"))
                assertEquals(size.toString(), url?.queryParameter("size"))
            }
        }
    }

    @Test
    fun `avatar preserves bearer precedence and stored session fallback`() {
        MockWebServer().use { server ->
            listOf(
                Triple("provider-token", "stored-session", "Bearer provider-token"),
                Triple(null, "stored-session", "Bearer stored-session"),
                Triple(" ", "stored-session", "Bearer stored-session"),
                Triple(null, null, null),
            ).forEach { (provider, stored, expected) ->
                server.enqueue(pngResponse())
                val store = FakeSessionCredentialStore(stored)
                val client = HolonHttpClient(
                    server.url("/").toString(),
                    bearerTokenProvider = BearerTokenProvider { provider },
                    sessionCredentialStore = store,
                )

                client.readAgentAvatar("agent_1")

                assertEquals(expected, server.takeRequest().getHeader("Authorization"))
                assertEquals(stored, store.value)
            }
        }
    }

    @Test
    fun `avatar credential storage failure never sends an unauthenticated request`() {
        MockWebServer().use { server ->
            val expected = IllegalStateException("credential storage unavailable")
            val store = object : SessionCredentialStore {
                override fun read(): String? = throw expected
                override fun write(credential: String) = Unit
                override fun clear() = Unit
            }
            val client = HolonHttpClient(server.url("/").toString(), sessionCredentialStore = store)

            val failure = assertFailsWith<IllegalStateException> { client.readAgentAvatar("agent_1") }

            assertTrue(failure === expected)
            assertEquals(0, server.requestCount)
        }
    }

    @Test
    fun `avatar safely encodes Unicode and reserved characters in the agent ID`() {
        MockWebServer().use { server ->
            server.enqueue(pngResponse())
            val client = HolonHttpClient(server.url("/api/").toString())
            val agentId = "机器人🤖 #?size=512&format=svg%"

            client.readAgentAvatar(agentId)

            val url = server.takeRequest().requestUrl
            assertEquals(listOf("api", "agents", agentId, "avatar"), url?.pathSegments)
            assertEquals(setOf("version", "expression", "format", "size"), url?.queryParameterNames)
            assertEquals("128", url?.queryParameter("size"))
            assertEquals("png", url?.queryParameter("format"))
            assertNull(url?.fragment)
        }
    }

    @Test
    fun `avatar rejects invalid sizes and agent IDs before sending a request`() {
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/").toString())
            listOf(Int.MIN_VALUE, -1, 0, 16, 33, 1024, Int.MAX_VALUE).forEach { size ->
                assertFailsWith<IllegalArgumentException> { client.readAgentAvatar("agent_1", size = size) }
            }
            listOf("", " ", "agent/other").forEach { agentId ->
                assertFailsWith<IllegalArgumentException> { client.readAgentAvatar(agentId) }
            }
            assertEquals(0, server.requestCount)
        }
    }

    @Test
    fun `avatar rejects missing or non-PNG content types`() {
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/").toString())
            listOf(null, "application/json", "image/svg+xml", "image/pngx", "text/plain").forEach { type ->
                val response = MockResponse().setBody("not a PNG")
                type?.let { response.setHeader("Content-Type", it) }
                server.enqueue(response)

                val failure = assertFailsWith<HolonProtocolException> { client.readAgentAvatar("agent_1") }

                assertEquals("Holon returned a non-PNG avatar", failure.message)
                server.takeRequest()
            }
        }
    }

    @Test
    fun `avatar rejects empty bodies`() {
        MockWebServer().use { server ->
            server.enqueue(pngResponse(byteArrayOf()))
            val client = HolonHttpClient(server.url("/").toString())

            val failure = assertFailsWith<HolonProtocolException> { client.readAgentAvatar("agent_1") }

            assertEquals("Holon returned an empty avatar", failure.message)
        }
    }

    @Test
    fun `avatar rejects an oversized declared body without reading it`() {
        MockWebServer().use { server ->
            server.enqueue(pngResponse().setHeader("Content-Length", 2 * 1024 * 1024 + 1))
            val client = HolonHttpClient(server.url("/").toString())

            val failure = assertFailsWith<HolonProtocolException> { client.readAgentAvatar("agent_1") }

            assertEquals("Holon avatar exceeds the 2 MiB limit", failure.message)
        }
    }

    @Test
    fun `avatar rejects oversized chunked bodies without a content length`() {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setHeader("Content-Type", "image/png")
                    .setChunkedBody(Buffer().write(ByteArray(2 * 1024 * 1024 + 1)), 8192),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val failure = assertFailsWith<HolonProtocolException> { client.readAgentAvatar("agent_1") }

            assertEquals("Holon avatar exceeds the 2 MiB limit", failure.message)
        }
    }

    @Test
    fun `avatar accepts exactly two MiB for fixed length and chunked bodies`() {
        MockWebServer().use { server ->
            val bytes = ByteArray(2 * 1024 * 1024) { (it % 251).toByte() }
            server.enqueue(pngResponse(bytes))
            server.enqueue(
                MockResponse()
                    .setHeader("Content-Type", "image/png")
                    .setChunkedBody(Buffer().write(bytes), 8192),
            )
            val client = HolonHttpClient(server.url("/").toString())

            repeat(2) {
                assertContentEquals(bytes, client.readAgentAvatar("agent_1"))
            }
        }
    }

    @Test
    fun `avatar HTTP failures preserve JSON errors session identity and later requests`() {
        MockWebServer().use { server ->
            val store = FakeSessionCredentialStore("stored-session")
            val client = HolonHttpClient(server.url("/").toString(), sessionCredentialStore = store)
            listOf(401, 404, 503).forEach { status ->
                server.enqueue(
                    jsonResponse(
                        """{"ok":false,"code":"avatar_error","error":"Cannot read avatar","context":{"detail":"original detail"}}""",
                        status,
                    ).setHeader("Retry-After", "3"),
                )
                val failure = assertFailsWith<HolonHttpException> { client.readAgentAvatar("agent_1") }

                assertEquals(status, failure.statusCode)
                assertEquals("avatar_error", failure.apiError?.code)
                assertEquals("Cannot read avatar", failure.apiError?.message)
                assertEquals("original detail", failure.apiError?.detail)
                assertEquals("agents/agent_1/avatar", failure.requestPath)
                assertEquals(client.instanceId, failure.clientInstanceId)
                assertEquals(if (status == 503) 3L else null, failure.retryAfterSeconds)
                assertEquals("stored-session", store.value)
                assertEquals("Bearer stored-session", server.takeRequest().getHeader("Authorization"))

                server.enqueue(pngResponse())
                assertContentEquals(byteArrayOf(1), client.readAgentAvatar("agent_1"))
                assertEquals("Bearer stored-session", server.takeRequest().getHeader("Authorization"))
            }
        }
    }

    @Test
    fun `avatar exposes only valid nonnegative Retry-After seconds on service unavailable`() {
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/").toString())
            listOf(
                "1" to 1L,
                "0" to 0L,
                " 12 " to 12L,
                Long.MAX_VALUE.toString() to Long.MAX_VALUE,
                null to null,
                "" to null,
                "-1" to null,
                "+1" to null,
                "1.5" to null,
                "9223372036854775808" to null,
                "later" to null,
                "Sun, 08 Nov 2026 10:00:00 GMT" to null,
            ).forEachIndexed { index, (header, expected) ->
                val response = jsonResponse("""{"ok":false,"code":"busy","error":"Try again"}""", 503)
                header?.let { response.setHeader("Retry-After", it) }
                server.enqueue(response)
                server.enqueue(pngResponse())

                val failure = assertFailsWith<HolonHttpException> { client.readAgentAvatar("agent_1") }

                assertEquals(503, failure.statusCode)
                assertEquals("busy", failure.apiError?.code)
                assertEquals("Try again", failure.apiError?.message)
                assertEquals("agents/agent_1/avatar", failure.requestPath)
                assertEquals(client.instanceId, failure.clientInstanceId)
                assertEquals(expected, failure.retryAfterSeconds, "Retry-After: $header")
                assertEquals(index * 2 + 1, server.requestCount, "No implicit retry for Retry-After: $header")
                server.takeRequest()

                assertContentEquals(byteArrayOf(1), client.readAgentAvatar("agent_1"))
                assertEquals(index * 2 + 2, server.requestCount)
                server.takeRequest()
            }
        }
    }

    @Test
    fun `avatar transport preserves inherited interceptors and authentication follow-ups`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse("""{"ok":false,"code":"unauthorized","error":"Authenticate"}""", 401)
                    .setHeader("WWW-Authenticate", "Bearer"),
            )
            server.enqueue(
                jsonResponse("""{"ok":false,"code":"busy","error":"Try again"}""", 503)
                    .setHeader("Retry-After", "0"),
            )
            server.enqueue(pngResponse())
            var applicationCalls = 0
            var networkCalls = 0
            var authenticationChallenges = 0
            val transport =
                HolonHttpClient.defaultHttpClient().newBuilder()
                    .addInterceptor { chain ->
                        applicationCalls++
                        chain.proceed(chain.request().newBuilder().header("X-Application", "inherited").build())
                    }
                    .addNetworkInterceptor { chain ->
                        networkCalls++
                        chain.proceed(chain.request().newBuilder().header("X-Network", "inherited").build())
                    }
                    .authenticator { _, response ->
                        authenticationChallenges++
                        response.request.newBuilder().header("Authorization", "Bearer challenged").build()
                    }
                    .build()
            val store = FakeSessionCredentialStore("stored-session")
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    bearerTokenProvider = BearerTokenProvider { "explicit-token" },
                    httpClient = transport,
                    sessionCredentialStore = store,
                    insecureHttpHosts = emptySet(),
                )

            val failure = assertFailsWith<HolonHttpException> { client.readAgentAvatar("agent_1") }

            assertEquals(503, failure.statusCode)
            assertEquals(0L, failure.retryAfterSeconds)
            assertEquals(2, server.requestCount)
            assertEquals(1, applicationCalls)
            assertEquals(2, networkCalls)
            assertEquals(1, authenticationChallenges)
            assertEquals("stored-session", store.value)
            listOf("Bearer explicit-token", "Bearer challenged").forEach { authorization ->
                val request = server.takeRequest()
                assertEquals(authorization, request.getHeader("Authorization"))
                assertEquals("inherited", request.getHeader("X-Application"))
                assertEquals("inherited", request.getHeader("X-Network"))
            }

            assertContentEquals(byteArrayOf(1), client.readAgentAvatar("agent_1"))
            assertEquals(3, server.requestCount)
            assertEquals(2, applicationCalls)
            assertEquals(3, networkCalls)
            assertEquals(1, authenticationChallenges)
            assertEquals("Bearer explicit-token", server.takeRequest().getHeader("Authorization"))
        }
    }

    @Test
    fun `Retry-After parsing is limited to avatar requests and old exception calls remain valid`() {
        assertNull(HolonHttpException(503, null).retryAfterSeconds)
        assertNull(HolonHttpException(503, null, "agents/list", "legacy-client").retryAfterSeconds)
        MockWebServer().use { server ->
            server.enqueue(jsonResponse("""{"ok":false,"code":"busy","error":"Try again"}""", 503).setHeader("Retry-After", "5"))
            val client = HolonHttpClient(server.url("/").toString())

            val failure = assertFailsWith<HolonHttpException> { client.listAgents() }

            assertNull(failure.retryAfterSeconds)
        }
    }

    @Test fun `content report posts authenticated request and decodes receipt`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse("""{"accepted":true,"report_id":"report_1","created_at":"2026-10-09T16:00:00.000Z"}"""))
            val client = HolonHttpClient(
                server.url("/").toString(),
                bearerTokenProvider = BearerTokenProvider { "session-token" },
            )
            val receipt = client.createContentReport(
                HolonContentReportRequest(
                    agentId = "agent_1",
                    turnId = "turn_1",
                    messageId = "transcript_1",
                    category = HolonContentReportCategory.PRIVACY,
                    description = "test",
                    clientRequestId = "retry-1",
                ),
            )
            val request = server.takeRequest(1, TimeUnit.SECONDS)
            assertEquals("/content-reports", request?.path)
            assertEquals("Bearer session-token", request?.getHeader("Authorization"))
            val body = HolonWire.json.parseToJsonElement(request?.body?.readUtf8().orEmpty()).jsonObject
            assertEquals("retry-1", body["client_request_id"]?.jsonPrimitive?.content)
            assertEquals("report_1", receipt.reportId)
            assertTrue(receipt.accepted)
        }
    }

    @Test fun `native exchange sends proof in body without authorizing the bootstrap request`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse("""{"credential":"session","ok":true,"user_id":"u","expires_at":null}"""))
            val client = HolonHttpClient(server.url("/").toString(), bearerTokenProvider = BearerTokenProvider { "previous-session" })
            client.exchangeSession("ticket", "native-proof")
            val request = server.takeRequest()
            assertEquals("POST", request.method)
            assertEquals("/auth/session/exchange/native", request.path)
            assertNull(request.getHeader("Authorization"))
            val body = HolonWire.json.parseToJsonElement(request.body.readUtf8()).jsonObject
            assertEquals("ticket", body["credential"]?.jsonPrimitive?.content)
            assertEquals("native-proof", body["native_verifier"]?.jsonPrimitive?.content)
        }
    }
    @Test
    fun `transport errors never erase credentials and identify their originating client`() {
        MockWebServer().use { server ->
            var saved = "session-A"
            val store = object : SessionCredentialStore {
                override fun read() = saved
                override fun write(credential: String) { saved = credential }
                override fun clear() { saved = "" }
            }
            val old = HolonHttpClient(server.url("/").toString(), sessionCredentialStore = store)
            server.enqueue(jsonResponse("""{"code":"forbidden","message":"Permission denied"}""").setResponseCode(401))
            val failure = assertFailsWith<HolonHttpException> { old.currentUser() }
            store.write("session-B")
            val replacement = HolonHttpClient(server.url("/").toString(), sessionCredentialStore = store)
            assertEquals("session-B", saved)
            assertEquals(old.instanceId, failure.clientInstanceId)
            assertTrue(replacement.instanceId != failure.clientInstanceId)
            assertTrue(!requiresSessionRenewal(401, "forbidden"))
            assertTrue(!requiresSessionRenewal(403, "auth_required"))
            assertTrue(requiresSessionRenewal(401, "session_expired_or_revoked"))
        }
    }
    @Test
    fun `credential storage read failures do not send an unauthenticated request`() {
        MockWebServer().use { server ->
            val expected = IllegalStateException("credential storage unavailable")
            val store =
                object : SessionCredentialStore {
                    override fun read(): String? = throw expected

                    override fun write(credential: String) = Unit

                    override fun clear() = Unit
                }
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    sessionCredentialStore = store,
                )

            assertFailsWith<IllegalStateException> { client.listAgents() }
            assertEquals(0, server.requestCount)
        }
    }

    @Test
    fun `current user keepalive uses the authenticated session endpoint`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    """{"ok":true,"user_id":"user-1","display_name":"Test User","auth_method":"local"}""",
                ),
            )
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    bearerTokenProvider = BearerTokenProvider { "test-session" },
                )

            assertEquals("user-1", client.currentUser().userId)

            val request = server.takeRequest()
            assertEquals("/auth/session/me", request.path)
            assertEquals("Bearer test-session", request.getHeader("Authorization"))
        }
    }

    @Test
    fun `model catalog parses available model metadata and availability`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse(fixture("models-v1.json")))
            val client = HolonHttpClient(server.url("/").toString())

            val catalog = client.modelCatalog()

            assertEquals(
                HolonModelOption(
                    model = "openai/gpt-5.6",
                    displayName = "GPT 5.6",
                    provider = "openai",
                    available = true,
                    supportsReasoningEffort = true,
                    reasoningEffortOptions = listOf("low", "high"),
                ),
                catalog.options.single(),
            )
            assertEquals("/models", server.takeRequest().path)
        }
    }

    @Test
    fun `agent model endpoints send operator authority and decode state`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    """{"runtime_default_model":"openai/gpt-5.6","source":"agent_override","effective_model":"anthropic/claude","active_model":"anthropic/claude","override_reasoning_effort":"high"}""",
                ),
            )
            server.enqueue(
                jsonResponse(
                    """{"runtime_default_model":"openai/gpt-5.6","source":"runtime_default","effective_model":"openai/gpt-5.6","active_model":"openai/gpt-5.6"}""",
                ),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val overridden = client.setAgentModel("agent-a", "anthropic/claude", "high")
            val cleared = client.clearAgentModel("agent-a")

            assertEquals("agent_override", overridden.source)
            assertEquals("anthropic/claude", overridden.effectiveModel)
            assertEquals("high", overridden.overrideReasoningEffort)
            assertEquals("runtime_default", cleared.source)
            assertEquals("openai/gpt-5.6", cleared.effectiveModel)

            val setRequest = server.takeRequest()
            assertEquals("/control/agents/agent-a/model", setRequest.path)
            val setBody = HolonWire.json.parseToJsonElement(setRequest.body.readUtf8()).jsonObject
            assertEquals("anthropic/claude", setBody["model"]?.jsonPrimitive?.content)
            assertEquals("high", setBody["reasoning_effort"]?.jsonPrimitive?.content)
            assertEquals("operator_instruction", setBody["authority_class"]?.jsonPrimitive?.content)

            val clearRequest = server.takeRequest()
            assertEquals("/control/agents/agent-a/model/clear", clearRequest.path)
            val clearBody = HolonWire.json.parseToJsonElement(clearRequest.body.readUtf8()).jsonObject
            assertEquals("operator_instruction", clearBody["authority_class"]?.jsonPrimitive?.content)
        }
    }

    @Test
    fun `brief read state endpoints use server cursor and exact unread count`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    """[{"agent_id":"agent-a","event_head_seq":12,"event_log_epoch":"epoch-1","oldest_retained_seq":3,"read_through_event_seq":8,"reset_required":false,"retention_gap":false,"revision":4,"unread_count":2,"visibility_scope_id":"scope-1"}]""",
                ),
            )
            server.enqueue(
                jsonResponse(
                    """{"agent_id":"agent-a","event_head_seq":12,"event_log_epoch":"epoch-1","oldest_retained_seq":3,"read_through_event_seq":8,"reset_required":false,"retention_gap":false,"revision":4,"unread_count":2,"visibility_scope_id":"scope-1"}""",
                ),
            )
            server.enqueue(
                jsonResponse(
                    """{"applied_read_through_event_seq":12,"state":{"agent_id":"agent-a","event_head_seq":12,"event_log_epoch":"epoch-1","oldest_retained_seq":3,"read_through_event_seq":12,"reset_required":false,"retention_gap":false,"revision":5,"unread_count":0,"visibility_scope_id":"scope-1"}}""",
                ),
            )
            val client = HolonHttpClient(
                server.url("/api/").toString(),
                bearerTokenProvider = BearerTokenProvider { "token" },
            )

            assertEquals(2, client.briefReadStates().single().unreadCount)
            assertEquals(8, client.briefReadState("agent-a").readThroughEventSeq)
            assertEquals(0, client.markBriefRead("agent-a", 12).state.unreadCount)

            assertEquals("/api/agents/brief-read-states", server.takeRequest().path)
            assertEquals("/api/agents/agent-a/brief-read-state", server.takeRequest().path)
            val mark = server.takeRequest()
            assertEquals("/api/agents/agent-a/brief-read-cursor", mark.path)
            assertEquals("""{"read_through_event_seq":12}""", mark.body.readUtf8())
            assertEquals("Bearer token", mark.getHeader("Authorization"))
        }
    }

    @Test
    fun `file references resolve through the authorized daemon and preserve root identity`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse("""{"results":[{"status":"resolved","location":{"workspace_id":"ws-1","execution_root_id":"root:old","path":"reports/final.md","absolute_path":"/host/reports/final.md","kind":"file","root_kind":"git_worktree_root"}}]}"""))
            server.enqueue(jsonResponse("""{"results":[{"status":"unresolved","reason":"not_found","message":"path does not exist"}]}"""))
            val client = HolonHttpClient(
                server.url("/api/").toString(),
                bearerTokenProvider = BearerTokenProvider { "session" },
            )

            val resolved = assertIs<HolonFileReferenceResult.Resolved>(
                client.resolveFileReference(HolonFileReference.WorkspaceUri("workspace://ws-1/reports/final.md?root=root%3Aold")),
            )
            assertEquals("root:old", resolved.location.executionRootId)
            assertEquals("reports/final.md", resolved.location.path)
            val request = server.takeRequest()
            assertEquals("/api/file-references/resolve", request.path)
            assertEquals("Bearer session", request.getHeader("Authorization"))
            assertEquals(
                """{"references":[{"type":"workspace_uri","workspace_uri":"workspace://ws-1/reports/final.md?root=root%3Aold"}]}""",
                request.body.readUtf8(),
            )

            assertIs<HolonFileReferenceResult.Unresolved>(
                client.resolveFileReference(HolonFileReference.AbsolutePath("/missing.txt")),
            )
            assertEquals(
                """{"references":[{"type":"absolute_path","absolute_path":"/missing.txt"}]}""",
                server.takeRequest().body.readUtf8(),
            )
        }
    }

    @Test
    fun `default client uses the bounded total call timeout`() {
        val client = HolonHttpClient.defaultHttpClient()
        assertEquals(90_000, client.callTimeoutMillis)
        assertEquals(10_000, client.connectTimeoutMillis)
    }

    @Test
    fun `default SSE client has no total call timeout`() {
        val client = HolonHttpClient.defaultSseHttpClient()

        assertEquals(0, client.callTimeoutMillis)
        assertEquals(45_000, client.readTimeoutMillis)
    }

    @Test
    fun `base URL requires HTTPS except for loopback`() {
        assertEquals(
            "https://holon.example/api/",
            HolonHttpClient.normalizeBaseUrl("https://holon.example/api").toString(),
        )
        assertEquals(
            "http://127.0.0.1:8787/",
            HolonHttpClient.normalizeBaseUrl("http://127.0.0.1:8787").toString(),
        )
        assertEquals(
            "http://[::1]:8787/",
            HolonHttpClient.normalizeBaseUrl("http://[::1]:8787").toString(),
        )
        assertEquals(
            "http://10.0.2.2:8787/",
            HolonHttpClient.normalizeBaseUrl(
                "http://10.0.2.2:8787",
                insecureHttpHosts = setOf("10.0.2.2"),
            ).toString(),
        )

        assertFailsWith<IllegalArgumentException> {
            HolonHttpClient.normalizeBaseUrl("http://192.0.2.10:8787")
        }
        assertFailsWith<IllegalArgumentException> {
            HolonHttpClient.normalizeBaseUrl("http://10.0.2.2:8787")
        }
        assertFailsWith<IllegalArgumentException> {
            HolonHttpClient.normalizeBaseUrl("http://127.attacker.example:8787")
        }
        assertFailsWith<IllegalArgumentException> {
            HolonHttpClient.normalizeBaseUrl("http://127.001.0.1:8787")
        }
        assertFailsWith<IllegalArgumentException> {
            HolonHttpClient.normalizeBaseUrl("https://token@holon.example")
        }
    }

    @Test
    fun `handshake sends bearer token and checks compatibility`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse(fixture("handshake-v1.json")))
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/proxy/").toString(),
                    bearerTokenProvider = BearerTokenProvider { "test-token" },
                )

            val result = client.handshake(setOf("agents.list"))

            val compatible = assertIs<CompatibilityResult.Compatible>(result)
            assertEquals("main", compatible.server.defaultAgentId)
            val request = server.takeRequest()
            assertEquals("/proxy/handshake", request.path)
            assertEquals("Bearer test-token", request.getHeader("Authorization"))
        }
    }

    @Test
    fun `handshake exposes daemon prompt limits`() {
        MockWebServer().use { server ->
            val response =
                fixture("handshake-v1.json").replace(
                    "\"future_handshake_field\"",
                    "\"limits\":{\"prompt_body_max_bytes\":1024," +
                        "\"prompt_file_attachment_max_bytes\":512," +
                        "\"prompt_image_attachment_max_bytes\":256}," +
                        "\"future_handshake_field\"",
                )
            server.enqueue(jsonResponse(response))
            val client = HolonHttpClient(server.url("/").toString())

            val compatible = assertIs<CompatibilityResult.Compatible>(client.handshake())

            assertEquals(1024L, compatible.server.limits?.promptBodyMaxBytes)
            assertEquals(512L, compatible.server.limits?.promptFileAttachmentMaxBytes)
            assertEquals(256L, compatible.server.limits?.promptImageAttachmentMaxBytes)
        }
    }

    @Test
    fun `agent roster decodes through generated DTOs and domain adapter`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse(fixture("agent-list-v1.json")))
            val client = HolonHttpClient(server.url("/").toString())

            val agents = client.listAgents()

            assertEquals(1, agents.size)
            assertEquals("main", agents.single().id)
            assertEquals("Primary", agents.single().displayName)
            assertEquals(1, agents.single().incarnation)
            assertEquals("/agents/list", server.takeRequest().path)
        }
    }

    @Test
    fun `agent incarnation is optional in constructors and preserves nullable wire values`() {
        val summary = AgentSummary(
            id = "main",
            displayName = "Primary",
            isDefault = true,
            registryStatus = "active",
            runtimeStatus = "awake_idle",
            effectiveModel = "openai/gpt-5.6",
            pending = 2,
            currentRunId = null,
        )
        assertNull(summary.incarnation)
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/").toString())
            for ((value, expected) in listOf("7" to 7, "null" to null)) {
                val body = fixture("agent-list-v1.json").replace(
                    "\"future_identity_field\"",
                    "\"incarnation\":$value,\"future_identity_field\"",
                )
                server.enqueue(jsonResponse(body))
                val agent = client.listAgents().single()
                assertEquals(expected, agent.incarnation)
                assertEquals("Primary", agent.displayName)
                assertEquals("/agents/list", server.takeRequest().path)
            }
        }
    }

    @Test
    fun `session exchange decodes and persists the native credential`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse(fixture("session-response-v1.json")))
            val store = FakeSessionCredentialStore()
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    sessionCredentialStore = store,
                )

            val session = client.exchangeSession("bootstrap-token")

            assertEquals("session-credential", session.credential)
            assertEquals("local-static-token", session.userId)
            assertEquals("2030-01-01T00:00:00Z", session.expiresAt)
            assertEquals("session-credential", store.value)
            val request = server.takeRequest()
            assertEquals("POST", request.method)
            assertEquals("/auth/session/exchange/native", request.path)
            assertEquals(
                """{"credential":"bootstrap-token"}""",
                request.body.readUtf8(),
            )
        }
    }

    @Test
    fun `pairing ticket redeems without forwarding an existing credential`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse(fixture("session-response-v1.json")))
            val store = FakeSessionCredentialStore().apply { write("old-session") }
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    sessionCredentialStore = store,
                )
            val ticket = "a".repeat(64)
            val session = client.redeemPairingTicket(ticket)

            assertEquals("session-credential", session.credential)
            assertEquals("session-credential", store.value)
            val request = server.takeRequest()
            assertEquals("/auth/pairing/redeem/native", request.path)
            assertNull(request.getHeader("Authorization"))
            assertEquals("""{"ticket":"$ticket"}""", request.body.readUtf8())
        }
    }

    @Test
    fun `stored session credential is used as bearer fallback`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse(fixture("agent-list-v1.json")))
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    sessionCredentialStore = FakeSessionCredentialStore("stored-session"),
                )

            client.listAgents()

            assertEquals("Bearer stored-session", server.takeRequest().getHeader("Authorization"))
        }
    }

    @Test
    fun `logout revokes the session and clears the store`() {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(HttpURLConnection.HTTP_NO_CONTENT))
            val store = FakeSessionCredentialStore("stored-session")
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    sessionCredentialStore = store,
                )

            client.logout()

            assertNull(store.value)
            val request = server.takeRequest()
            assertEquals("POST", request.method)
            assertEquals("/auth/session/logout", request.path)
            assertEquals("Bearer stored-session", request.getHeader("Authorization"))
        }
    }

    @Test
    fun `logout prefers the stored session over the explicit bearer provider`() {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(HttpURLConnection.HTTP_NO_CONTENT))
            val store = FakeSessionCredentialStore("stored-session")
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    bearerTokenProvider = BearerTokenProvider { "bootstrap-token" },
                    sessionCredentialStore = store,
                )

            client.logout()

            assertEquals("Bearer stored-session", server.takeRequest().getHeader("Authorization"))
            assertNull(store.value)
        }
    }

    @Test
    fun `logout clears the store when remote revocation fails`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    fixture("error-v1.json"),
                    HttpURLConnection.HTTP_INTERNAL_ERROR,
                ),
            )
            val store = FakeSessionCredentialStore("stored-session")
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    sessionCredentialStore = store,
                )

            assertFailsWith<HolonHttpException> { client.logout() }

            assertNull(store.value)
            assertEquals("Bearer stored-session", server.takeRequest().getHeader("Authorization"))
        }
    }

    @Test
    fun `unauthorized response leaves invalidation to the session owner`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    fixture("error-v1.json"),
                    HttpURLConnection.HTTP_UNAUTHORIZED,
                ),
            )
            val store = FakeSessionCredentialStore("expired-session")
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    sessionCredentialStore = store,
                )

            assertFailsWith<HolonHttpException> { client.listAgents() }

            assertEquals("expired-session", store.value)
            assertEquals("Bearer expired-session", server.takeRequest().getHeader("Authorization"))
        }
    }

    @Test
    fun `machine error is exposed without leaking an undecodable body`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    fixture("error-v1.json"),
                    HttpURLConnection.HTTP_UNAUTHORIZED,
                ),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val error = assertFailsWith<HolonHttpException> { client.listAgents() }

            assertEquals(HttpURLConnection.HTTP_UNAUTHORIZED, error.statusCode)
            assertEquals("invalid_json", error.apiError?.code)
            assertEquals("unknown field `kind`", error.apiError?.detail)
        }

        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setResponseCode(HttpURLConnection.HTTP_BAD_GATEWAY)
                    .setBody("upstream failed"),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val error = assertFailsWith<HolonHttpException> { client.listAgents() }

            assertEquals(HttpURLConnection.HTTP_BAD_GATEWAY, error.statusCode)
            assertNull(error.apiError)
            assertEquals("Holon request failed with HTTP 502", error.message)
        }
    }

    @Test
    fun `artifact keeps the server byte size for multibyte content`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    """{"artifact_index":2,"size":4,"content":"éé"}""",
                ),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val artifact = client.artifact("main", "tool-1", 2)

            assertEquals(2, artifact.artifactIndex)
            assertEquals(4, artifact.size)
            assertEquals("éé", artifact.content)
        }
    }

    @Test
    fun `artifact rejects responses missing required fields`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse("""{"artifact_index":2,"content":"ok"}"""))
            val client = HolonHttpClient(server.url("/").toString())

            val error = assertFailsWith<HolonProtocolException> {
                client.artifact("main", "tool-1", 2)
            }

            assertEquals("Holon artifact response is missing size", error.message)
        }
    }

    @Test
    fun `artifact rejects a mismatched response index`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse("""{"artifact_index":3,"size":2,"content":"ok"}"""))
            val client = HolonHttpClient(server.url("/").toString())

            val error = assertFailsWith<HolonProtocolException> {
                client.artifact("main", "tool-1", 2)
            }

            assertEquals(
                "Holon artifact response index 3 does not match requested index 2",
                error.message,
            )
        }
    }

    @Test
    fun `artifact rejects negative sizes and out of range indexes`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse("""{"artifact_index":2,"size":-1,"content":"ok"}"""))
            server.enqueue(
                jsonResponse(
                    """{"artifact_index":4294967298,"size":2,"content":"ok"}""",
                ),
            )
            val client = HolonHttpClient(server.url("/").toString())

            assertEquals(
                "Holon artifact response size must be non-negative",
                assertFailsWith<HolonProtocolException> {
                    client.artifact("main", "tool-1", 2)
                }.message,
            )
            assertEquals(
                "Holon artifact response is missing artifact_index",
                assertFailsWith<HolonProtocolException> {
                    client.artifact("main", "tool-1", 2)
                }.message,
            )
        }
    }

    @Test
    fun `prompt sends stable client request id and decodes duplicate receipt`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    """{"ok":true,"agent_id":"main","message_id":"msg-1","disposition":"duplicate"}""",
                ),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val result = client.sendOperatorPrompt("main", "continue", "request-1")

            assertEquals("msg-1", result.messageId)
            assertEquals("duplicate", result.disposition)
            val request = server.takeRequest()
            assertEquals("/control/agents/main/prompt", request.path)
            assertEquals(
                "request-1",
                HolonWire.json.parseToJsonElement(request.body.readUtf8())
                    .let { it as kotlinx.serialization.json.JsonObject }["client_request_id"]
                    ?.let { it as kotlinx.serialization.json.JsonPrimitive }
                    ?.content,
            )
        }
    }

    @Test
    fun `workspace artifact locator uses authorized workspace download route`() {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setHeader("Content-Type", "text/plain; charset=utf-8")
                    .setBody("finished"),
            )
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    sessionCredentialStore = FakeSessionCredentialStore("session"),
                )

            val artifact = client.downloadWorkspaceArtifact(
                "workspace://ws-one/reports/final%20note.txt?root=root%3Aws-one",
            )

            assertContentEquals("finished".encodeToByteArray(), artifact.bytes)
            assertEquals("text/plain", artifact.mediaType)
            assertEquals("final note.txt", artifact.fileName)
            val request = server.takeRequest()
            assertEquals(
                "/workspaces/ws-one/files/reports/final%20note.txt?download=true&execution_root_id=root%3Aws-one",
                request.path,
            )
            assertEquals("Bearer session", request.getHeader("Authorization"))
        }
    }

    @Test
    fun `workspace artifact streams to a file without buffering the response`() {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setHeader("Content-Type", "text/plain").setChunkedBody("finished", 2))
            val client = HolonHttpClient(
                baseUrl = server.url("/").toString(),
                sessionCredentialStore = FakeSessionCredentialStore("session"),
            )
            val target = kotlin.io.path.createTempFile("holon-artifact", ".txt").toFile()
            try {
                val downloaded = client.downloadWorkspaceArtifactToFile(
                    "workspace://ws-one/reports/final.txt?root=root%3Aws-one",
                    target,
                    maxBytes = 16,
                )
                assertEquals("finished", target.readText())
                assertEquals(8, downloaded.size)
                assertEquals("text/plain", downloaded.mediaType)
                assertEquals("Bearer session", server.takeRequest().getHeader("Authorization"))
            } finally {
                target.delete()
            }
        }
    }

    @Test
    fun `workspace artifact stream stops at the configured byte limit`() {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setChunkedBody("finished", 2))
            val client = HolonHttpClient(server.url("/").toString())
            val target = kotlin.io.path.createTempFile("holon-artifact-limit", ".txt").toFile()
            try {
                assertFailsWith<HolonProtocolException> {
                    client.downloadWorkspaceFileToFile("ws-one", "report.txt", target, maxBytes = 4)
                }
                assertTrue(target.length() <= 4)
            } finally {
                target.delete()
            }
        }
    }

    @Test
    fun `conversation detail keeps assistant text and tool identity`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    """{"activities":[{"kind":"assistant","id":"assistant:a","revision":2,"summary":"done","key":{"event_seq":7}},{"kind":"tool","id":"tool:tool-1","revision":1,"summary":"Read · success","key":{"event_seq":8}}],"coverage":{"kind":"partial","reason":"unknown_activity_type"},"has_more":false,"next_before_cursor":null}""",
                ),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val detail = client.conversationDetail("main", "turn-1", limit = 100)

            assertEquals("done", detail.activities.first().summary)
            assertEquals("tool-1", detail.activities.last().toolExecutionId)
            assertEquals("partial", detail.coverageKind)
            assertEquals("unknown_activity_type", detail.coverageReason)
            assertEquals("/agents/main/turns/turn-1/activities?limit=100", server.takeRequest().path)
        }
    }

    @Test
    fun `agent workspaces preserve execution root identity and directory listing`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    """{"workspace":{"workspaces":[{"workspace_id":"ws-1","repo_name":"holon","is_active":true,"execution_root_id":"root:one","projection_kind":"git_worktree_root"}]}}""",
                ),
            )
            server.enqueue(
                jsonResponse(
                    """{"type":"directory","workspace_id":"ws-1","execution_root_id":"root:one","path":"apps","root_kind":"git_worktree_root","entries":[{"name":"android","type":"directory","size":0},{"name":"README.md","type":"file","size":42,"mime_type":"text/markdown"}]}""",
                ),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val workspace = client.agentWorkspaces("main").single()
            val listing = client.browseWorkspaceDirectory(workspace.workspaceId, "apps", workspace.executionRootId)

            assertEquals("root:one", workspace.executionRootId)
            assertEquals(listOf("android", "README.md"), listing.entries.map { it.name })
            server.takeRequest()
            assertEquals(
                "/workspaces/ws-1/files/apps?execution_root_id=root%3Aone",
                server.takeRequest().path,
            )
        }
    }

    @Test
    fun `work item detail projects plan todos and result`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    """{"id":"work-1","state":"completed","objective":"Ship Android","readiness":"completed","revision":3,"result_brief_id":"brief-1","result_summary":"green","plan_artifact":{"owner_agent_id":"main","workspace_id":"agent_home:main","relative_path":"work-items/work-1/plan.md","bytes":4096,"preview":"steps","preview_complete":false},"todo_list":[{"text":"test","state":"completed"}],"work_refs":[{"kind":"file","ref":"README.md","title":"README","status":"active"}]}""",
                ),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val item = client.workItemSnapshot("main", "work-1")

            assertEquals("green", item.resultSummary)
            assertEquals("steps", item.planArtifact?.preview)
            assertEquals("main", item.planArtifact?.ownerAgentId)
            assertEquals("agent_home:main", item.planArtifact?.workspaceId)
            assertEquals("work-items/work-1/plan.md", item.planArtifact?.relativePath)
            assertEquals(4096, item.planArtifact?.bytes)
            assertEquals(false, item.planArtifact?.previewComplete)
            assertEquals("test", item.todoList.single().text)
            assertEquals("README.md", item.workRefs.single().ref)
            assertEquals("/agents/main/work-items/work-1", server.takeRequest().path)
        }
    }

    @Test
    fun `abort current run keeps the agent available for the next prompt`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse("{}"))
            val client = HolonHttpClient(server.url("/api/").toString())

            client.abortCurrentRun("holon tester", "run-42")

            val request = server.takeRequest()
            assertEquals(
                "/api/control/agents/holon%20tester/current-run/abort",
                request.path,
            )
            assertEquals(
                """{"run_id":"run-42","mode":"idle_after_abort","authority_class":"operator_instruction"}""",
                request.body.readUtf8(),
            )
        }
    }

    @Test
    fun `reconnecting conversation stream retries connection failures`() {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AT_START))
            server.enqueue(
                MockResponse()
                    .setHeader("Content-Type", "text/event-stream")
                    .setBody("id: event-1\ndata: {\"event_seq\":1}\n\n"),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val events =
                client
                    .reconnectingConversationStream(
                        agentId = "main",
                        policy =
                            SseReconnectPolicy(
                                maxAttempts = 1,
                                initialDelayMillis = 0,
                                maxDelayMillis = 0,
                            ),
                    ).toList()

            assertEquals(listOf("event-1"), events.map { it.id })
            assertEquals(2, server.requestCount)
        }
    }

    @Test
    fun `reconnecting conversation stream does not retry HTTP errors`() {
        MockWebServer().use { server ->
            server.enqueue(
                jsonResponse(
                    fixture("error-v1.json"),
                    HttpURLConnection.HTTP_UNAUTHORIZED,
                ),
            )
            val client = HolonHttpClient(server.url("/").toString())

            assertFailsWith<HolonHttpException> {
                client
                    .reconnectingConversationStream(
                        agentId = "main",
                        policy =
                            SseReconnectPolicy(
                                maxAttempts = 2,
                                initialDelayMillis = 0,
                                maxDelayMillis = 0,
                            ),
                    ).toList()
            }

            assertEquals(1, server.requestCount)
        }
    }

    @Test
    fun `reconnecting conversation stream retries transient HTTP errors`() {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(HttpURLConnection.HTTP_UNAVAILABLE))
            server.enqueue(
                MockResponse()
                    .setHeader("Content-Type", "text/event-stream")
                    .setBody("id: recovered\ndata: {\"event_seq\":1}\n\n"),
            )
            val client = HolonHttpClient(server.url("/").toString())

            val events =
                client
                    .reconnectingConversationStream(
                        agentId = "main",
                        policy =
                            SseReconnectPolicy(
                                maxAttempts = 1,
                                initialDelayMillis = 0,
                                maxDelayMillis = 0,
                            ),
                    ).toList()

            assertEquals(listOf("recovered"), events.map { it.id })
            assertEquals(2, server.requestCount)
        }
    }

    @Test
    fun `event listener factory and sse retry observer are notified`() {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(HttpURLConnection.HTTP_UNAVAILABLE))
            server.enqueue(MockResponse().setResponseCode(HttpURLConnection.HTTP_UNAVAILABLE))
            val startedCalls = java.util.concurrent.CopyOnWriteArrayList<String>()
            val retries = java.util.concurrent.CopyOnWriteArrayList<Triple<String, Int, Long>>()
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    eventListenerFactory =
                        okhttp3.EventListener.Factory {
                            object : okhttp3.EventListener() {
                                override fun callStart(call: okhttp3.Call) {
                                    startedCalls.add(call.request().url.encodedPath)
                                }
                            }
                        },
                    sseRetryObserver =
                        SseRetryObserver { path, attempt, delay ->
                            retries.add(Triple(path, attempt, delay))
                        },
                )

            assertFailsWith<HolonHttpException> {
                client
                    .reconnectingConversationStream(
                        agentId = "main",
                        policy =
                            SseReconnectPolicy(
                                maxAttempts = 1,
                                initialDelayMillis = 0,
                                maxDelayMillis = 0,
                            ),
                    ).toList()
            }

            assertEquals(2, startedCalls.size)
            assertTrue(retries.isNotEmpty())
            assertTrue(retries.all { it.first == "agents/main/conversation/stream" && it.second >= 1 })
        }
    }

    @Test
    fun `reconnecting conversation stream reports transient error after retries are exhausted`() {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(HttpURLConnection.HTTP_UNAVAILABLE))
            server.enqueue(MockResponse().setResponseCode(HttpURLConnection.HTTP_UNAVAILABLE))
            val client = HolonHttpClient(server.url("/").toString())

            val error =
                assertFailsWith<HolonHttpException> {
                    client
                        .reconnectingConversationStream(
                            agentId = "main",
                            policy =
                                SseReconnectPolicy(
                                    maxAttempts = 1,
                                    initialDelayMillis = 0,
                                    maxDelayMillis = 0,
                                ),
                        ).toList()
                }

            assertEquals(HttpURLConnection.HTTP_UNAVAILABLE, error.statusCode)
            assertEquals(2, server.requestCount)
        }
    }

    @Test
    fun `redirects are rejected without forwarding bearer credentials`() {
        MockWebServer().use { redirectTarget ->
            MockWebServer().use { server ->
                server.enqueue(
                    MockResponse()
                        .setResponseCode(HttpURLConnection.HTTP_MOVED_TEMP)
                        .setHeader("Location", redirectTarget.url("/agents/list")),
                )
                val client =
                    HolonHttpClient(
                        baseUrl = server.url("/").toString(),
                        bearerTokenProvider = BearerTokenProvider { "test-token" },
                    )

                val error = assertFailsWith<HolonHttpException> { client.listAgents() }

                assertEquals(HttpURLConnection.HTTP_MOVED_TEMP, error.statusCode)
                assertEquals("Bearer test-token", server.takeRequest().getHeader("Authorization"))
                assertEquals(0, redirectTarget.requestCount)
            }
        }
    }

    @Test
    fun `request call timeout is surfaced as a protocol error`() {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setBody("{}")
                    .setBodyDelay(200, TimeUnit.MILLISECONDS),
            )
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/").toString(),
                    bearerTokenProvider = BearerTokenProvider { null },
                    httpClient =
                        OkHttpClient.Builder()
                            .callTimeout(20, TimeUnit.MILLISECONDS)
                            .build(),
                        sessionCredentialStore = null,
                    insecureHttpHosts = emptySet(),
                )

            val error = assertFailsWith<HolonProtocolException> { client.listAgents() }

            assertEquals("Holon request failed", error.message)
        }
    }

    private fun pngResponse(body: ByteArray = byteArrayOf(1)): MockResponse =
        MockResponse()
            .setHeader("Content-Type", "image/png")
            .setBody(Buffer().write(body))

    private fun jsonResponse(
        body: String,
        status: Int = HttpURLConnection.HTTP_OK,
    ): MockResponse =
        MockResponse()
            .setResponseCode(status)
            .setHeader("Content-Type", "application/json")
            .setBody(body)

    private class FakeSessionCredentialStore(initialValue: String? = null) : SessionCredentialStore {
        var value: String? = initialValue

        override fun read(): String? = value

        override fun write(credential: String) {
            value = credential
        }

        override fun clear() {
            value = null
        }
    }
}
