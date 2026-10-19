package run.holon.android.app

import android.webkit.WebView
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import run.holon.android.sdk.HolonHttpException

/**
 * Opt-in isolated daemon check. Never run with a production runtime or device.
 * The private fixture file supplies only a disposable local daemon bootstrap.
 */
@RunWith(AndroidJUnit4::class)
class RuntimeBrowserDaemonIntegrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()

    @Test fun nativeLoginAuthenticatesAppSdkAndBrowserLogoutRevokesNativeSession() {
        assumeTrue(InstrumentationRegistry.getArguments().getString("runtimeBrowserDaemon") == "true")
        val context = instrumentation.targetContext
        val fixture = JSONObject(context.filesDir.resolve("runtime-browser-fixture.json").readText())
        val baseUrl = fixture.getString("baseUrl")
        require(baseUrl.startsWith("http://127.0.0.1:")) { "Only an adb-reversed loopback fixture is permitted" }
        val container = main { AppContainer(context) }
        val repository = container.repository
        runBlocking { repository.login(baseUrl, fixture.getString("bootstrap").toCharArray(), allowInsecureHttp = true) }
        val lease = repository.sessions.capture()
        val url = resolveRuntimeWebLink("/apps/demo-android/browser-check/", lease.session.baseUrl)!!
        val authFailures = AtomicInteger()
        fun open() = main {
            RuntimeWebPage(
                context, url, lease.session.baseUrl,
                isCurrent = { repository.sessions.current === lease },
                bindCookie = { write -> repository.bindRuntimeBrowserCookie(lease, url, true, write) },
                onExternal = { fail("Fixture must not navigate externally") },
                onAuthenticationFailure = { authFailures.incrementAndGet() },
            )
        }
        var page: RuntimeWebPage? = null
        try {
            val first = open()
            page = first
            awaitJs(first.view, "window.verification?.context && window.verification?.request && window.verification?.event")
            assertEquals("false", js(first.view, "document.cookie.includes('holon_session')"))
            repository.validateRuntimeBrowserSession(lease)
            main { first.close() }
            // Closing retires the browser profile, not the native session.
            repository.validateRuntimeBrowserSession(lease)
            val second = open()
            page = second
            awaitJs(second.view, "window.verification?.context && window.verification?.request && window.verification?.event")
            js(second.view, """
                fetch('/api/auth/session/logout', {method: 'POST'}).then(r => { window.loggedOut = r.status === 204; }); true
            """.trimIndent())
            awaitJs(second.view, "window.loggedOut === true")
            val failure = runCatching { repository.validateRuntimeBrowserSession(lease) }.exceptionOrNull()
            assertTrue("Browser logout must revoke the native session", failure is HolonHttpException && failure.statusCode == 401)
            main { second.reload() }
            await { authFailures.get() == 1 }
        } finally {
            main { page?.close() }
            runBlocking { repository.logout() }
            context.filesDir.resolve("runtime-browser-fixture.json").delete()
        }
    }

    private fun js(view: WebView, script: String): String {
        val done = CountDownLatch(1)
        var result = ""
        main { view.evaluateJavascript(script) { result = it; done.countDown() } }
        assertTrue(done.await(10, TimeUnit.SECONDS))
        return result
    }

    private fun awaitJs(view: WebView, expression: String) = await { js(view, expression) == "true" }

    private fun await(condition: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(40)
        while (System.nanoTime() < deadline) {
            if (condition()) return
            Thread.sleep(100)
        }
        fail("Daemon App SDK condition did not complete")
    }

    private fun <T> main(action: () -> T): T {
        var result: Result<T>? = null
        instrumentation.runOnMainSync { result = runCatching(action) }
        return result!!.getOrThrow()
    }
}
