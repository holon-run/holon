package run.holon.android.app

import android.webkit.WebView
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class RuntimeBrowserSessionIntegrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()

    @Test fun sessionAuthenticatesHtmlFetchEventsAndWorkerButIsNotReadableByScript() {
        val authenticated = mutableSetOf<String>()
        MockWebServer().use { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    if (request.getHeader("Cookie")?.contains("holon_session=session-marker") != true) {
                        return MockResponse().setResponseCode(401)
                    }
                    synchronized(authenticated) { authenticated += request.path.orEmpty() }
                    return when (request.path) {
                        "/asset.js" -> MockResponse().setHeader("Content-Type", "application/javascript")
                            .setBody("window.assetReady = true;")
                        "/fetch" -> MockResponse().setBody("ok")
                        "/events" -> MockResponse().setHeader("Content-Type", "text/event-stream")
                            .setBody("data: ready\n\n")
                        "/worker.js" -> MockResponse().setHeader("Content-Type", "application/javascript").setBody(
                            """
                            self.addEventListener('install', e => e.waitUntil(self.skipWaiting()));
                            self.addEventListener('activate', e => e.waitUntil(self.clients.claim()));
                            self.addEventListener('message', e => {
                              e.waitUntil(fetch('/worker-fetch').then(r => r.text()).then(t => e.ports[0].postMessage(t)));
                            });
                            """.trimIndent(),
                        )
                        "/worker-fetch" -> MockResponse().setBody("worker-ok")
                        else -> MockResponse().setHeader("Content-Type", "text/html").setBody(
                            """
                            <!doctype html><script src="/asset.js"></script><script>
                            (async () => {
                              window.fetchReady = await (await fetch('/fetch')).text() === 'ok';
                              const events = new EventSource('/events');
                              events.onmessage = () => { window.eventsReady = true; events.close(); };
                              await navigator.serviceWorker.register('/worker.js');
                              const reg = await navigator.serviceWorker.ready;
                              const channel = new MessageChannel();
                              channel.port1.onmessage = e => { window.workerReady = e.data === 'worker-ok'; };
                              reg.active.postMessage('fetch', [channel.port2]);
                            })();
                            </script>
                            """.trimIndent(),
                        )
                    }
                }
            }
            server.start()
            val page = create(server.url("/").toString())
            try {
                awaitJs(page.view, "window.assetReady && window.fetchReady && window.eventsReady && window.workerReady")
                assertEquals("false", js(page.view, "document.cookie.includes('session-marker')"))
                assertEquals("false", js(page.view, "JSON.stringify(localStorage).includes('session-marker')"))
                synchronized(authenticated) {
                    assertTrue(authenticated.containsAll(setOf("/", "/asset.js", "/fetch", "/events", "/worker.js", "/worker-fetch")))
                }
            } finally { main { page.close() } }
        }
    }

    @Test fun retiredLeaseCannotCompleteDelayedCookieBindingOrReload() {
        MockWebServer().use { server ->
            server.start()
            val current = AtomicBoolean(true)
            var delayedWrite: ((String) -> Unit)? = null
            val url = server.url("/").toString()
            val page = create(url, isCurrent = current::get, bind = { delayedWrite = it })
            try {
                current.set(false)
                main { delayedWrite!!(runtimeSessionCookie("late-marker", url)); page.reload() }
                assertNull("A retired callback must not start a request", server.takeRequest(750, TimeUnit.MILLISECONDS))
            } finally { main { page.close(); page.close() } }
        }
    }

    @Test fun mainFrame401InvalidatesSessionBut403DoesNotLoopLogin() {
        for (status in listOf(401, 403)) {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setResponseCode(status))
                server.start()
                val failures = AtomicInteger()
                val page = create(server.url("/").toString(), onAuth = { failures.incrementAndGet() })
                try {
                    await { main { page.error != null } }
                    assertEquals(if (status == 401) 1 else 0, failures.get())
                    assertEquals(1, server.requestCount)
                } finally { main { page.close() } }
            }
        }
    }

    @Test fun explicitExternalNavigationDoesNotSendSessionAndCloseDoesNotLogout() {
        MockWebServer().use { server ->
            MockWebServer().use { external ->
                server.enqueue(MockResponse().setHeader("Content-Type", "text/html").setBody("<!doctype html>ready"))
                server.start()
                external.start()
                val routed = CountDownLatch(1)
                val url = server.url("/").toString()
                val page = create(url, onExternal = { routed.countDown() })
                try {
                    await { main { !page.loading } }
                    val destination = external.url("/").newBuilder().host("127.0.0.1").build().toString()
                    js(page.view, "location.href = '$destination'; true")
                    assertTrue(routed.await(10, TimeUnit.SECONDS))
                    assertNull(external.takeRequest(750, TimeUnit.MILLISECONDS))
                } finally { main { page.close() } }
                val requests = (0 until server.requestCount).mapNotNull { server.takeRequest(1, TimeUnit.SECONDS)?.path }
                assertFalse(requests.any { it.contains("logout") })
            }
        }
    }

    private fun create(
        url: String,
        isCurrent: () -> Boolean = { true },
        bind: (((String) -> Unit) -> Unit)? = null,
        onAuth: () -> Unit = {},
        onExternal: (String) -> Unit = {},
    ): RuntimeWebPage = main {
        RuntimeWebPage(
            instrumentation.targetContext, url, url, isCurrent,
            bindCookie = bind ?: { write -> write(runtimeSessionCookie("session-marker", url)) },
            onExternal = onExternal, onAuthenticationFailure = onAuth,
        )
    }

    private fun js(view: WebView, expression: String): String {
        val done = CountDownLatch(1)
        var result = ""
        main { view.evaluateJavascript(expression) { result = it; done.countDown() } }
        assertTrue(done.await(10, TimeUnit.SECONDS))
        return result
    }

    private fun awaitJs(view: WebView, expression: String) = await { js(view, expression) == "true" }

    private fun await(condition: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(20)
        while (System.nanoTime() < deadline) {
            if (condition()) return
            Thread.sleep(50)
        }
        fail("Browser condition did not complete")
    }

    private fun <T> main(action: () -> T): T {
        var result: Result<T>? = null
        instrumentation.runOnMainSync { result = runCatching(action) }
        return result!!.getOrThrow()
    }
}
