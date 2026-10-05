package run.holon.android.sdk

import java.security.MessageDigest
import java.util.Base64
import kotlin.test.assertEquals
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue
import org.junit.Test

class NativeLoginProofTest {
    @Test fun `fresh native proof is S256 and never contains padding`() {
        val proof = NativeLoginProof.create()
        assertEquals(43, proof.verifier.length)
        assertTrue(proof.verifier.matches(Regex("[A-Za-z0-9_-]{43}")))
        assertEquals(Base64.getUrlEncoder().withoutPadding().encodeToString(
            MessageDigest.getInstance("SHA-256").digest(proof.verifier.toByteArray()),
        ), proof.challenge)
        assertNotEquals(proof.verifier, NativeLoginProof.create().verifier)
        assertNotEquals(proof.verifier, proof.challenge)
    }
}
