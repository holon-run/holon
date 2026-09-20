package run.holon.android.sdk

import java.net.ServerSocket
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
        val port = ServerSocket(0).use { it.localPort }
        val token = "android-sdk-integration-token"
        val process =
            ProcessBuilder(
                binary.toString(),
                "serve",
                "--listen",
                "127.0.0.1:$port",
                "--token",
                token,
            )
                .redirectErrorStream(true)
                .redirectOutput(log.toFile())
                .apply {
                    environment()["HOLON_HOME"] = home.toString()
                }
                .start()

        try {
            val client =
                HolonHttpClient(
                    baseUrl = "http://127.0.0.1:$port/api",
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
}
