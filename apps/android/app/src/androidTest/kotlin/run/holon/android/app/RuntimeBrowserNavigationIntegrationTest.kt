package run.holon.android.app

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** Standard cookie semantics, not port isolation. Uses only disposable markers. */
@RunWith(AndroidJUnit4::class)
class RuntimeBrowserNavigationIntegrationTest {
    @Test fun sameHostCrossPortRedirectUsesStandardCookieScope() {
        redirectedCookie(stayOnHost = true)
    }

    @Test fun crossHostSubresourceRedirectDoesNotReceiveSession() {
        redirectedCookie(stayOnHost = false)
    }

    private fun redirectedCookie(stayOnHost: Boolean) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        assertTrue(RuntimeBrowserProfiles.isSupported())
        MockWebServer().use { runtime ->
            MockWebServer().use { otherPort ->
                otherPort.enqueue(MockResponse().setBody("not a runtime"))
                otherPort.start()
                val redirected = CountDownLatch(1)
                runtime.dispatcher = object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse =
                        if (request.path == "/redirect") {
                            redirected.countDown()
                            MockResponse().setResponseCode(302)
                                .setHeader("Location", otherPort.url("/collector").newBuilder()
                                    .host(if (stayOnHost) "localhost" else "127.0.0.1").build())
                        } else {
                            MockResponse().setHeader("Content-Type", "text/html")
                                .setBody("<!doctype html><img src='/redirect'>")
                        }
                }
                runtime.start()
                val url = runtime.url("/").toString()
                lateinit var page: RuntimeWebPage
                instrumentation.runOnMainSync {
                    page = RuntimeWebPage(
                        context = instrumentation.targetContext,
                        initialUrl = url,
                        baseUrl = url,
                        isCurrent = { true },
                        bindCookie = { write -> write(runtimeSessionCookie("redirect-gate-marker", url)) },
                        onExternal = {},
                        onAuthenticationFailure = {},
                    )
                }
                try {
                    assertTrue("The runtime subresource must load", redirected.await(10, TimeUnit.SECONDS))
                    val collected = otherPort.takeRequest(5, TimeUnit.SECONDS)
                    assertNotNull("Redirect must reach the collector, not pass by being blocked", collected)
                    val receivedSession = collected!!.getHeader("Cookie")?.contains("redirect-gate-marker") == true
                    if (stayOnHost) {
                        assertTrue("Cookies intentionally do not isolate ports on a trusted host", receivedSession)
                    } else {
                        assertFalse("Host-only session must not reach another host", receivedSession)
                    }
                } finally {
                    instrumentation.runOnMainSync { page.close() }
                }
            }
        }
    }
}
