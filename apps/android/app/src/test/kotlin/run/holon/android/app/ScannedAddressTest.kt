package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class ScannedAddressTest {
    @Test
    fun `normalizes supported Holon address`() {
        assertEquals(
            "https://holon.example/api",
            normalizeScannedAddress("https://holon.example/api/"),
        )
        assertEquals(
            "http://100.64.0.1:7878",
            normalizeScannedAddress("http://100.64.0.1:7878"),
        )
        assertEquals(
            "https://holon.example/api",
            normalizeScannedAddress("HTTPS://holon.example/api/"),
        )
        assertEquals(
            "http://100.64.0.1:7878",
            normalizeScannedAddress("HTTP://100.64.0.1:7878"),
        )
    }

    @Test
    fun `rejects credentials and non HTTP schemes`() {
        assertFailsWith<IllegalArgumentException> {
            normalizeScannedAddress("javascript:alert(1)")
        }
        assertFailsWith<IllegalArgumentException> {
            normalizeScannedAddress("https://user:pass@holon.example")
        }
        assertFailsWith<IllegalArgumentException> {
            normalizeScannedAddress("https://holon.example?token=secret")
        }
    }
}
