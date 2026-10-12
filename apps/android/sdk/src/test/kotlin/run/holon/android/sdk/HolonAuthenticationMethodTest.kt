package run.holon.android.sdk

import java.net.HttpURLConnection
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull

class HolonAuthenticationMethodTest {
    @Test
    fun `authentication method decodes both supported modes`() {
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/api").toString())
            val modes =
                mapOf(
                    "local" to HolonAuthenticationMethod.LocalToken,
                    "oidc" to HolonAuthenticationMethod.Oidc,
                )
            modes.forEach { (mode, expected) ->
                server.enqueue(jsonResponse("""{"mode":"$mode","future_field":true}"""))

                assertEquals(expected, client.authenticationMethod())

                val request = server.takeRequest()
                assertEquals("GET", request.method)
                assertEquals("/api/auth/method", request.path)
                assertEquals(0L, request.bodySize)
            }
        }
    }

    @Test
    fun `authentication method preserves the configured API base path`() {
        MockWebServer().use { server ->
            val paths =
                mapOf(
                    "/" to "/auth/method",
                    "/api" to "/api/auth/method",
                    "/api/" to "/api/auth/method",
                    "/tenant/api" to "/tenant/api/auth/method",
                )
            paths.forEach { (basePath, expectedPath) ->
                server.enqueue(jsonResponse("""{"mode":"oidc"}"""))

                HolonHttpClient(server.url(basePath).toString()).authenticationMethod()

                assertEquals(expectedPath, server.takeRequest().path)
            }
        }
    }

    @Test
    fun `authentication method never reads or sends existing credentials or cookies`() {
        MockWebServer().use { server ->
            var credentialAccesses = 0
            val store =
                object : SessionCredentialStore {
                    override fun read(): String {
                        credentialAccesses++
                        return "existing-session"
                    }

                    override fun write(credential: String) {
                        credentialAccesses++
                    }

                    override fun clear() {
                        credentialAccesses++
                    }
                }
            val client =
                HolonHttpClient(
                    baseUrl = server.url("/api/").toString(),
                    bearerTokenProvider =
                        BearerTokenProvider {
                            credentialAccesses++
                            "existing-bearer"
                        },
                    sessionCredentialStore = store,
                )
            repeat(2) {
                server.enqueue(
                    jsonResponse("""{"mode":"oidc"}""")
                        .setHeader("Set-Cookie", "session=existing-cookie; Path=/"),
                )

                assertEquals(HolonAuthenticationMethod.Oidc, client.authenticationMethod())

                val request = server.takeRequest()
                assertNull(request.getHeader("Authorization"))
                assertNull(request.getHeader("Cookie"))
                assertNull(request.getHeader("Proxy-Authorization"))
            }
            assertEquals(0, credentialAccesses)
        }
    }

    @Test
    fun `authentication method reports HTTP failures without fallback`() {
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/api/").toString())
            listOf(HttpURLConnection.HTTP_UNAUTHORIZED, HttpURLConnection.HTTP_UNAVAILABLE).forEach { status ->
                server.enqueue(jsonResponse("""{"mode":"local"}""").setResponseCode(status))

                val error = assertFailsWith<HolonHttpException> { client.authenticationMethod() }

                assertEquals(status, error.statusCode)
                assertEquals("auth/method", error.requestPath)
                assertEquals(client.instanceId, error.clientInstanceId)
            }
        }
    }

    @Test
    fun `authentication method rejects unknown and missing modes`() {
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/api/").toString())
            listOf("""{"mode":"saml"}""", """{"mode":""}""", """{"mode":"local_token"}""", "{}").forEach { body ->
                server.enqueue(jsonResponse(body))

                assertFailsWith<HolonProtocolException> { client.authenticationMethod() }
            }
        }
    }

    @Test
    fun `authentication method rejects wrong mode and response types`() {
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/api/").toString())
            val bodies =
                listOf(
                    """{"mode":null}""",
                    """{"mode":true}""",
                    """{"mode":42}""",
                    """{"mode":[]}""",
                    """{"mode":{}}""",
                    "null",
                    "[]",
                    """"oidc"""",
                    "42",
                )
            bodies.forEach { body ->
                server.enqueue(jsonResponse(body))

                assertFailsWith<HolonProtocolException> { client.authenticationMethod() }
            }
        }
    }

    @Test
    fun `authentication method rejects malformed and non JSON responses`() {
        MockWebServer().use { server ->
            val client = HolonHttpClient(server.url("/api/").toString())
            listOf("", "not JSON", "<html>Login</html>", """{"mode":""").forEach { body ->
                server.enqueue(MockResponse().setBody(body))

                assertFailsWith<HolonProtocolException> { client.authenticationMethod() }
            }
        }
    }

    @Test
    fun `authentication method rejects 308 without following the redirect`() {
        MockWebServer().use { redirectTarget ->
            MockWebServer().use { server ->
                redirectTarget.enqueue(jsonResponse("""{"mode":"oidc"}"""))
                server.enqueue(
                    MockResponse()
                        .setResponseCode(308)
                        .setHeader("Location", redirectTarget.url("/api/auth/method")),
                )
                val client =
                    HolonHttpClient(
                        baseUrl = server.url("/api/").toString(),
                        bearerTokenProvider = BearerTokenProvider { "existing-bearer" },
                    )

                val error = assertFailsWith<HolonHttpException> { client.authenticationMethod() }

                assertEquals(308, error.statusCode)
                val request = server.takeRequest()
                assertEquals("/api/auth/method", request.path)
                assertNull(request.getHeader("Authorization"))
                assertNull(request.getHeader("Cookie"))
                assertEquals(1, server.requestCount)
                assertEquals(0, redirectTarget.requestCount)
            }
        }
    }

    private fun jsonResponse(body: String): MockResponse =
        MockResponse()
            .setHeader("Content-Type", "application/json")
            .setBody(body)
}
