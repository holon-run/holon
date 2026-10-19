package run.holon.android.app

import android.annotation.SuppressLint
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.webkit.WebViewCompat
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Assume.assumeFalse
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

/** Provider gate uses disposable test markers only, never a real Holon credential. */
@SuppressLint("SetJavaScriptEnabled", "RequiresFeature")
@RunWith(AndroidJUnit4::class)
class RuntimeBrowserProfileIntegrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context get() = instrumentation.targetContext

    /** Invoked in two separate instrumentation processes by the daemon verification script. */
    @Test fun coldStartDeletesAbandonedProfileWithoutLoadingIt() {
        val phase = InstrumentationRegistry.getArguments().getString("runtimeBrowserColdStart")
        assumeTrue(phase == "seed" || phase == "check")
        val evidence = context.getSharedPreferences("runtime_browser_cold_start_test", 0)
        if (phase == "seed") {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setHeader("Content-Type", "text/html").setBody("<!doctype html>seed"))
                server.start()
                lateinit var abandoned: RuntimeBrowserProfile
                lateinit var browser: WebView
                onMain {
                    abandoned = RuntimeBrowserProfiles.create(context)
                    browser = view(abandoned)
                    assertTrue(evidence.edit().putString("abandoned", abandoned.profile.name).commit())
                }
                load(browser, server.url("/").toString())
                js(browser, "localStorage.setItem('abandoned', 'marker'); true")
                // Chromium persists its profile registry asynchronously, independently of cookies.
                // Seed an actually persisted profile before testing next-process deletion.
                Thread.sleep(12_000)
                onMain {
                    abandoned.profile.cookieManager.flush()
                    browser.destroy()
                    // Deliberately do not retire: the harness force-stops this process.
                }
            }
        } else {
            onMain {
                val name = checkNotNull(evidence.getString("abandoned", null))
                val store = androidx.webkit.ProfileStore.getInstance()
                assertTrue("Seeded profile must exist before cold cleanup: ${store.allProfileNames}", store.allProfileNames.contains(name))
                RuntimeBrowserProfiles.cleanupOnColdStart(context)
                assertFalse(store.allProfileNames.any { it.startsWith(RUNTIME_BROWSER_PROFILE_PREFIX) })
                assertTrue(evidence.edit().clear().commit())
            }
        }
    }

    @Test fun unsupportedProviderRefusesIdentityContainer() {
        assumeFalse(RuntimeBrowserProfiles.isSupported())
        onMain {
            assertTrue(runCatching { RuntimeBrowserProfiles.create(context) }.isFailure)
        }
    }

    @Test fun isolatedProfilesClearCookiesStorageAndActiveServiceWorkers() {
        assertTrue("Provider must support MULTI_PROFILE and DELETE_BROWSING_DATA", RuntimeBrowserProfiles.isSupported())
        MockWebServer().use { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest) =
                    if (request.path == "/worker.js") {
                        MockResponse().setHeader("Content-Type", "application/javascript").setBody(
                            """
                            self.addEventListener('install', e => e.waitUntil(self.skipWaiting()));
                            self.addEventListener('activate', e => e.waitUntil(self.clients.claim()));
                            self.addEventListener('message', e => {
                              e.ports[0].postMessage('started');
                              e.waitUntil(new Promise(resolve => setTimeout(async () => {
                                await (await caches.open('late-worker')).put('/late', new Response('old'));
                                resolve();
                              }, 400)));
                            });
                            """.trimIndent(),
                        )
                    } else {
                        MockResponse().setHeader("Content-Type", "text/html").setBody("<!doctype html><title>gate</title>")
                    }
            }
            server.start()
            val url = server.url("/").toString()
            lateinit var a: RuntimeBrowserProfile
            lateinit var b: RuntimeBrowserProfile
            lateinit var first: WebView
            lateinit var second: WebView
            onMain {
                a = RuntimeBrowserProfiles.create(context)
                b = RuntimeBrowserProfiles.create(context)
                first = view(a)
                second = view(b)
            }
            try {
                val cookieWritten = CountDownLatch(1)
                onMain { a.profile.cookieManager.setCookie(url, runtimeSessionCookie("gate-marker", url)) { cookieWritten.countDown() } }
                assertTrue(cookieWritten.await(10, TimeUnit.SECONDS))
                load(first, url)
                js(first, """
                    (async () => {
                      localStorage.setItem('marker', 'old');
                      await new Promise((resolve, reject) => {
                        const r = indexedDB.open('marker', 1); r.onupgradeneeded = () => r.result.createObjectStore('data');
                        r.onsuccess = () => { r.result.close(); resolve(); }; r.onerror = reject;
                      });
                      await (await caches.open('marker')).put('/marker', new Response('old'));
                      await navigator.serviceWorker.register('/worker.js');
                      await navigator.serviceWorker.ready;
                      window.gateReady = true;
                    })(); true
                """.trimIndent())
                awaitJs(first, "window.gateReady === true")
                assertEquals("false", js(first, "document.cookie.includes('gate-marker')"))
                load(second, url)
                assertNull(b.profile.cookieManager.getCookie(url))
                assertEquals("null", js(second, "localStorage.getItem('marker')"))
                checkStorage(second, empty = true)
                // Keep a worker task in flight at retirement, rather than checking idle storage only.
                js(first, """
                    (async () => {
                      const reg = await navigator.serviceWorker.ready;
                      const channel = new MessageChannel();
                      channel.port1.onmessage = () => { window.workerStarted = true; };
                      reg.active.postMessage('late', [channel.port2]);
                    })(); true
                """.trimIndent())
                awaitJs(first, "window.workerStarted === true")
                onMain { first.stopLoading(); first.destroy(); a.retire() }
                awaitCondition { a.cleanupComplete }
                Thread.sleep(900)
                // Test-only forensic probe of a retired marker profile; production never reuses it.
                lateinit var probe: WebView
                onMain { probe = view(a) }
                try {
                    load(probe, url)
                    assertNull(a.profile.cookieManager.getCookie(url))
                    assertEquals("null", js(probe, "localStorage.getItem('marker')"))
                    checkStorage(probe, empty = true)
                } finally { onMain { probe.destroy() } }
            } finally {
                onMain {
                    if (!a.retired) { first.destroy(); a.retire() }
                    second.destroy()
                    b.retire()
                }
            }
        }
    }

    private fun view(profile: RuntimeBrowserProfile) = WebView(context).apply {
        WebViewCompat.setProfile(this, profile.profile.name)
        settings.javaScriptEnabled = true
        settings.domStorageEnabled = true
    }

    private fun load(view: WebView, url: String) {
        val done = CountDownLatch(1)
        onMain {
            view.webViewClient = object : WebViewClient() {
                override fun onPageFinished(view: WebView, url: String) { done.countDown() }
            }
            view.loadUrl(url)
        }
        assertTrue("Page did not finish", done.await(15, TimeUnit.SECONDS))
    }

    private fun checkStorage(view: WebView, empty: Boolean) {
        js(view, """
            (async () => {
              window.storageEmpty = (await indexedDB.databases()).length === 0 &&
                (await caches.keys()).length === 0 && (await navigator.serviceWorker.getRegistrations()).length === 0;
            })(); true
        """.trimIndent())
        awaitJs(view, "window.storageEmpty === $empty")
    }

    private fun awaitJs(view: WebView, expression: String) = awaitCondition { js(view, expression) == "true" }

    private fun awaitCondition(condition: () -> Boolean) {
        val end = System.nanoTime() + TimeUnit.SECONDS.toNanos(15)
        while (System.nanoTime() < end) {
            if (condition()) return
            Thread.sleep(50)
        }
        fail("Browser gate condition did not complete")
    }

    private fun js(view: WebView, script: String): String {
        val done = CountDownLatch(1)
        var result = ""
        onMain { view.evaluateJavascript(script) { result = it; done.countDown() } }
        assertTrue(done.await(10, TimeUnit.SECONDS))
        return result
    }

    private fun onMain(action: () -> Unit) = instrumentation.runOnMainSync(action)
}
