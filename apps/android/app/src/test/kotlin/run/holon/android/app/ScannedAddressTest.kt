package run.holon.android.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class ScannedAddressTest {
    private val ticket = "a".repeat(64)

    @Test
    fun `accepts menu pairing QR without leaking ticket into the address`() {
        assertEquals(
            ScannedPairing("https://holon.example", ticket),
            parseScannedPairing("https://holon.example/login#pair=$ticket"),
        )
        assertEquals(
            ScannedPairing("http://100.64.0.1:7878", ticket),
            parseScannedPairing("http://100.64.0.1:7878/login#pair=$ticket"),
        )
    }

    @Test
    fun `rejects malformed pairing QR`() {
        listOf(
            "https://holon.example/login?ticket=x#pair=$ticket",
            "https://user@holon.example/login#pair=$ticket",
            "https://holon.example/other#pair=$ticket",
            "https://holon.example/login#pair=short",
            "https://holon.example/login#pair=$ticket&extra=1",
            "ftp://holon.example/login#pair=$ticket",
        ).forEach { assertFailsWith<IllegalArgumentException> { parseScannedPairing(it) } }
    }

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
