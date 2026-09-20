package run.holon.android.sdk

import java.nio.file.Files
import java.nio.file.Path
import java.time.Duration
import java.util.concurrent.TimeUnit
import org.junit.Test
import kotlin.io.path.readText
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertTrue

class HolonHttpClientDaemonTest {
    @Test
    fun `handshake and roster work against a real daemon`() {
        val binary = Path.of(requireNotNull(System.getProperty("holon.test.binary")))
        val home = Files.createTempDirectory("holon-android-sdk-")
        val log = home.resolve("daemon.log")
        val token = "android-sdk-integration-token"
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
            val client =
                HolonHttpClient(
                    baseUrl = "http://$address/api",
                    bearerTokenProvider = BearerTokenProvider { token },
                )
            val compatibility =
                awaitDaemon(process, log) {
                    client.handshake(setOf("agents.list"))
                }
            val compatible = assertIs<CompatibilityResult.Compatible>(compatibility)
            assertEquals("bearer", compatible.server.authMode)
            assertTrue(compatible.server.authRequired)

            val agents = client.listAgents()
            assertTrue(agents.any { it.id == compatible.server.defaultAgentId })
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

    private fun awaitDaemonAddress(
        process: Process,
        log: Path,
    ): String {
        val deadline = System.nanoTime() + Duration.ofSeconds(20).toNanos()
        while (System.nanoTime() < deadline) {
            check(process.isAlive) {
                "Holon daemon exited before binding:\n${log.readText()}"
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
        throw AssertionError("Holon daemon did not bind:\n${log.readText()}")
    }

    private fun <T> awaitDaemon(
        process: Process,
        log: Path,
        request: () -> T,
    ): T {
        val deadline = System.nanoTime() + Duration.ofSeconds(20).toNanos()
        var lastFailure: Throwable? = null
        while (System.nanoTime() < deadline) {
            check(process.isAlive) {
                "Holon daemon exited before becoming ready:\n${log.readText()}"
            }
            try {
                return request()
            } catch (failure: HolonProtocolException) {
                lastFailure = failure
                Thread.sleep(100)
            }
        }
        throw AssertionError(
            "Holon daemon did not become ready:\n${log.readText()}",
            lastFailure,
        )
    }

    private companion object {
        const val LISTENING_PREFIX = "Holon listening on "
        const val TEST_MODEL = "openai/gpt-5.4"
        const val TEST_OPENAI_API_KEY = "android-sdk-integration-test-key"
    }
}
