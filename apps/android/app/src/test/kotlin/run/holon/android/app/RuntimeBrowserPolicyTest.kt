package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class RuntimeBrowserPolicyTest {
    @Test fun `cookie uses host only HttpOnly session semantics and actual transport security`() {
        assertEquals(
            "holon_session=marker; Path=/; HttpOnly; SameSite=Lax; Secure",
            runtimeSessionCookie("marker", "https://holon.test:8443/apps/a/b/"),
        )
        val http = runtimeSessionCookie("marker", "http://127.0.0.1:8080/apps/a/b/")
        assertFalse(http.contains("Secure"))
        assertFalse(http.contains("Domain"))
        assertFalse(http.contains("Max-Age"))
        assertFalse(http.contains("Expires"))
    }

    @Test fun `origin includes scheme port and normalized host not API path`() {
        assertTrue(sameRuntimeOrigin("https://HOLON.test:443/apps/a/b/", "https://holon.test/api/"))
        assertTrue(sameRuntimeOrigin("http://[::1]:8080/apps/a/b/", "http://[::1]:8080/api/"))
        assertFalse(sameRuntimeOrigin("http://holon.test/apps/a/b/", "https://holon.test/api/"))
        assertFalse(sameRuntimeOrigin("https://holon.test:8443/apps/a/b/", "https://holon.test/api/"))
        assertFalse(sameRuntimeOrigin("https://holon.test.evil/apps/a/b/", "https://holon.test/api/"))
    }

    @Test fun `navigation rejects unsafe targets and never treats another port as internal`() {
        val base = "https://holon.test/api/"
        listOf("javascript:alert(1)", "file:///apps/a/b", "content://settings",
            "https://user:secret@holon.test/", "https://@holon.test/", "https://holon.test/\\evil",
            "https://holon.test/\n", "https://holon.test/%zz", "//holon.test/apps/a/b",
        ).forEach { assertEquals(RuntimeBrowserNavigation.Blocked, runtimeBrowserNavigation(it, base), it) }
        assertEquals(RuntimeBrowserNavigation.Internal, runtimeBrowserNavigation("https://holon.test/anything", base))
        assertEquals(RuntimeBrowserNavigation.External, runtimeBrowserNavigation("https://holon.test:8443/", base))
        assertEquals(RuntimeBrowserNavigation.External, runtimeBrowserNavigation("http://holon.test/", base))
        assertEquals(RuntimeBrowserNavigation.External, runtimeBrowserNavigation("https://external.test/", base))
    }
}
