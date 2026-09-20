package run.holon.android.sdk

import java.net.HttpURLConnection
import java.util.concurrent.TimeUnit
import okhttp3.OkHttpClient
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
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
}
