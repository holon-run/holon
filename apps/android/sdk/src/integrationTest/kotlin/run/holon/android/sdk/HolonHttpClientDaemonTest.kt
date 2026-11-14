package run.holon.android.sdk

import java.nio.file.Files
import java.nio.file.Path
import java.net.HttpURLConnection
import java.net.URI
import java.io.ByteArrayInputStream
import java.time.Duration
import java.util.concurrent.TimeUnit
import javax.imageio.ImageIO
import org.junit.Test
import kotlin.io.path.readText
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertTrue

class HolonHttpClientDaemonTest {
    @Test
    fun `handshake roster and authenticated avatars work against a real daemon`() {
        val binary = Path.of(requireNotNull(System.getProperty("holon.test.binary")))
        val home = Files.createTempDirectory("holon-android-sdk-")
        val log = home.resolve("daemon.log")
        val token = TEST_BEARER_TOKEN
        val process =
            ProcessBuilder(
                binary.toString(),
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--token",
                token,
            )
                .redirectErrorStream(true)
                .redirectOutput(log.toFile())
                .apply {
                    environment()["HOLON_HOME"] = home.toString()
                    environment()["HOLON_MODEL"] = TEST_MODEL
                    // Startup validates provider availability; this test only
                    // exercises HTTP handshake/roster and never calls a model.
                    environment()["OPENAI_API_KEY"] = TEST_OPENAI_API_KEY
                }
                .start()

        try {
            val address = awaitDaemonAddress(process, log)
            val createMain =
                URI("http://$address/api/control/agents/main/create")
                    .toURL()
                    .openConnection() as HttpURLConnection
            try {
                createMain.requestMethod = "POST"
                createMain.connectTimeout = 5_000
                createMain.readTimeout = 5_000
                createMain.doOutput = true
                createMain.setRequestProperty("Authorization", "Bearer $token")
                createMain.setRequestProperty("Content-Type", "application/json")
                createMain.outputStream.use { it.write("{}".toByteArray()) }
                assertEquals(HttpURLConnection.HTTP_ACCEPTED, createMain.responseCode)
            } finally {
                createMain.disconnect()
            }
            val client =
                HolonHttpClient(
                    baseUrl = "http://$address/api",
                    bearerTokenProvider = BearerTokenProvider { token },
                )
            val compatibility =
                awaitDaemon({ process.isAlive }, log) {
                    client.handshake(setOf("agents.list", "agents.roster-snapshot.v1"))
                }
            val compatible = assertIs<CompatibilityResult.Compatible>(compatibility)
            assertEquals("bearer", compatible.server.authMode)
            assertTrue(compatible.server.authRequired)

            val expectedAgentId = compatible.server.defaultAgentId
            fun awaitAgents(
                endpoint: String,
                request: () -> List<AgentSummary>,
            ): List<AgentSummary> =
                awaitDaemon(
                    { process.isAlive },
                    log,
                    expectation = "$endpoint agent ids containing [$expectedAgentId]",
                    isReady = { agents -> agents.any { it.id == expectedAgentId } },
                    describe = { agents -> "agent ids ${agents.map { it.id }.sorted()}" },
                    request = request,
                )

            // Creation is asynchronous; handshake readiness does not publish membership.
            val agents = awaitAgents("/agents/snapshot") { client.rosterSnapshot().agents }
            assertTrue(agents.any { it.id == expectedAgentId })
            // Keep real-daemon coverage of the separate list API as well.
            val listedAgents = awaitAgents("/agents/list") { client.listAgents() }
            assertTrue(listedAgents.any { it.id == expectedAgentId })
            assertTrue("agent_avatar_v1" in compatible.server.capabilities)
            for (expression in HolonAvatarExpression.entries) {
                val png = client.readAgentAvatar(compatible.server.defaultAgentId, expression, 64)
                assertTrue(png.size in 1..(2 * 1024 * 1024))
                val image = ImageIO.read(ByteArrayInputStream(png))
                assertEquals(64, image.width)
                assertEquals(64, image.height)
            }
            val anonymous = HolonHttpClient(baseUrl = "http://$address/api")
            assertEquals(401, assertFailsWith<HolonHttpException> {
                anonymous.readAgentAvatar(compatible.server.defaultAgentId)
            }.statusCode)
        } finally {
            process.destroy()
            if (!process.waitFor(5, TimeUnit.SECONDS)) {
                process.destroyForcibly()
                assertTrue(
                    process.waitFor(5, TimeUnit.SECONDS),
                    "Holon daemon did not exit after forced termination",
                )
            }
            assertTrue(
                home.toFile().deleteRecursively(),
                "Failed to delete Holon daemon test directory: $home",
            )
        }
    }

    @Test
    fun `daemon readiness waits for membership and retries explicit observer contention`() {
        var elapsed = 0L
        var attempts = 0
        val agents =
            awaitDaemon(
                isAlive = { true },
                log = Path.of("unused-daemon.log"),
                expectation = "agent ids containing [main]",
                isReady = { ids: List<String> -> "main" in ids },
                nanoTime = { elapsed },
                pause = { elapsed += Duration.ofSeconds(1).toNanos() },
            ) {
                when (++attempts) {
                    1 -> emptyList()
                    2 -> listOf("other")
                    3 -> throw observerFailure(code = "projection_stale_unhandled")
                    4 -> throw observerFailure()
                    else -> listOf("main", "other")
                }
            }
        assertEquals(listOf("main", "other"), agents)
        assertEquals(5, attempts)
    }

    @Test
    fun `daemon readiness fails immediately on authentication protocol and unknown errors`() {
        val failures =
            listOf(
                observerFailure(status = 401),
                observerFailure(status = 404),
                observerFailure(status = 500),
                observerFailure(retryable = false),
                observerFailure(code = "unknown"),
                HolonHttpException(503, null),
                HolonProtocolException("malformed roster"),
                java.io.IOException("transport failure"),
            )
        for (failure in failures) {
            var attempts = 0
            var elapsed = 0L
            val error =
                assertFailsWith<AssertionError> {
                    awaitDaemon(
                        isAlive = { true },
                        log = Path.of("unused-daemon.log"),
                        nanoTime = { elapsed },
                        pause = { elapsed += Duration.ofSeconds(20).toNanos() },
                    ) {
                        attempts++
                        throw failure
                    }
                }
            assertEquals(1, attempts)
            assertEquals(0L, elapsed)
            assertEquals(failure, error.cause)
        }
    }

    @Test
    fun `daemon readiness deadline reports expected actual ids last error and sanitized log tail`() {
        val log = Files.createTempFile("holon-sdk-readiness-", ".log")
        try {
            Files.writeString(
                log,
                "old log line\n".repeat(50) +
                    "$TEST_BEARER_TOKEN $TEST_OPENAI_API_KEY Authorization: Bearer secret-value\n",
            )
            var elapsed = 0L
            var attempts = 0
            val failure = observerFailure()
            val error =
                assertFailsWith<AssertionError> {
                    awaitDaemon(
                        isAlive = { true },
                        log = log,
                        expectation = "agent ids containing [main]",
                        isReady = { ids: List<String> -> "main" in ids },
                        describe = { ids -> "agent ids $ids" },
                        nanoTime = { elapsed },
                        pause = { elapsed += Duration.ofSeconds(10).toNanos() },
                    ) {
                        if (++attempts == 1) listOf("other") else throw failure
                    }
                }
            val message = requireNotNull(error.message)
            assertTrue(message.contains("expected agent ids containing [main]"))
            assertTrue(message.contains("actual agent ids [other]"))
            assertTrue(message.contains("runtime_db_busy"))
            assertTrue(message.contains("daemon log tail"))
            assertTrue(message.contains("[REDACTED]"))
            assertTrue(!message.contains(TEST_BEARER_TOKEN))
            assertTrue(!message.contains(TEST_OPENAI_API_KEY))
            assertTrue(!message.contains("secret-value"))
            assertEquals(failure, error.cause)
            assertEquals(2, attempts)
        } finally {
            Files.deleteIfExists(log)
        }
    }

    private fun observerFailure(
        status: Int = 503,
        code: String = "runtime_db_busy",
        retryable: Boolean = true,
    ): HolonHttpException =
        HolonHttpException(
            status,
            HolonApiError(code, "observer read unavailable", retryable, null, null, emptyMap()),
        )

    private fun awaitDaemonAddress(
        process: Process,
        log: Path,
    ): String {
        val deadline = System.nanoTime() + Duration.ofSeconds(20).toNanos()
        while (System.nanoTime() < deadline) {
            check(process.isAlive) {
                "Holon daemon exited before binding; daemon log tail:\n${daemonLogTail(log)}"
            }
            val address =
                log.readText()
                    .lineSequence()
                    .firstNotNullOfOrNull { line ->
                        line.removePrefix(LISTENING_PREFIX).takeIf { it != line }
                    }
            if (address != null) {
                return address
            }
            Thread.sleep(100)
        }
        throw AssertionError("Holon daemon did not bind; daemon log tail:\n${daemonLogTail(log)}")
    }

    private fun <T> awaitDaemon(
        isAlive: () -> Boolean,
        log: Path,
        expectation: String = "a successful response",
        isReady: (T) -> Boolean = { true },
        describe: (T) -> String = { it.toString() },
        nanoTime: () -> Long = System::nanoTime,
        pause: () -> Unit = { Thread.sleep(100) },
        request: () -> T,
    ): T {
        val deadline = nanoTime() + Duration.ofSeconds(20).toNanos()
        var lastFailure: Exception? = null
        var lastObserved = "<no successful response>"
        fun readinessFailure(reason: String): AssertionError =
            AssertionError(
                "Holon daemon $reason; expected $expectation; actual $lastObserved; " +
                    "last error: ${lastFailure?.let { "${it.javaClass.simpleName}: ${it.message}" } ?: "<none>"}" +
                    "\ndaemon log tail:\n${daemonLogTail(log)}",
                lastFailure,
            )
        while (nanoTime() < deadline) {
            if (!isAlive()) {
                throw readinessFailure("exited before becoming ready")
            }
            try {
                val response = request()
                lastObserved = describe(response)
                if (isReady(response) && nanoTime() < deadline) return response
            } catch (failure: Exception) {
                val apiError = (failure as? HolonHttpException)?.apiError
                val observerBusy = failure is HolonHttpException && failure.statusCode == 503 &&
                    apiError?.retryable == true &&
                    apiError.code in setOf("runtime_db_busy", "projection_stale_unhandled")
                lastFailure = failure
                if (!observerBusy) throw readinessFailure("read failed")
            }
            if (nanoTime() < deadline) pause()
        }
        throw readinessFailure("did not become ready before the deadline")
    }

    private fun daemonLogTail(log: Path): String =
        runCatching {
            log.readText()
                .replace(TEST_BEARER_TOKEN, "[REDACTED]")
                .replace(TEST_OPENAI_API_KEY, "[REDACTED]")
                .replace(Regex("(?i)bearer\\s+[^\\s\"']+"), "Bearer [REDACTED]")
                .takeLast(16 * 1024).lines().takeLast(40).joinToString("\n")
        }.getOrElse { "<log unavailable: ${it.javaClass.simpleName}>" }

    private companion object {
        const val LISTENING_PREFIX = "Holon listening on "
        const val TEST_MODEL = "openai/gpt-5.4"
        const val TEST_OPENAI_API_KEY = "android-sdk-integration-test-key"
        const val TEST_BEARER_TOKEN = "android-sdk-integration-token"
    }
}
