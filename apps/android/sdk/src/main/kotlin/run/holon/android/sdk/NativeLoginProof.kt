package run.holon.android.sdk

import java.security.MessageDigest
import java.security.SecureRandom
import java.util.Base64

/** Native callback binding, separate from daemon/provider PKCE. */
public class NativeLoginProof private constructor(public val verifier: String) {
    public val challenge: String = Base64.getUrlEncoder().withoutPadding()
        .encodeToString(MessageDigest.getInstance("SHA-256").digest(verifier.toByteArray(Charsets.US_ASCII)))

    public companion object {
        public fun create(): NativeLoginProof = NativeLoginProof(
            Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32).also { SecureRandom().nextBytes(it) }),
        )
    }
}
