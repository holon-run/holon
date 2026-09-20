package run.holon.android.sdk

import java.net.HttpURLConnection
import java.util.concurrent.TimeUnit
import okhttp3.OkHttpClient
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.SocketPolicy
import org.junit.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNull

class HolonHttpClientTest {
    @Test
    fun `default client uses the bounded total call timeout`() {
        assertEquals(30_000, HolonHttpClient.defaultHttpClient().callTimeoutMillis)
    }

    @Test
    fun `default SSE client has no total call timeout`() {
        assertEquals(0, HolonHttpClient.defaultSseHttpClient().callTimeoutMillis)
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

        assertFailsWith<IllegalArgumentException> {
            HolonHttpClient.normalizeBaseUrl("http://192.0.2.10:8787")
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
    fun `agent roster decodes through generated DTOs and domain adapter`() {
        MockWebServer().use { server ->
            server.enqueue(jsonResponse(fixture("agent-list-v1.json")))
            val client = HolonHttpClient(server.url("/").toString())

            val agents = client.listAgents()

            assertEquals(1, agents.size)
            assertEquals("main", agents.single().id)
            assertEquals("Primary", agents.single().displayName)
            assertEquals("/agents/list", server.takeRequest().path)
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
    fun `unauthorized response clears the stored session credential`() {
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

            assertNull(store.value)
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
                )

            val error = assertFailsWith<HolonProtocolException> { client.listAgents() }

            assertEquals("Holon request failed", error.message)
        }
    }

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
