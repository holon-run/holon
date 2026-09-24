package run.holon.android.app

import android.content.Context
import android.content.SharedPreferences
import android.util.Base64
import java.nio.charset.StandardCharsets
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import run.holon.android.sdk.SessionCredentialStore

private const val PREFS = "holon_debug_session"
private const val SESSION_KEY = "session_credential"
private const val KEYSTORE = "AndroidKeyStore"
private const val KEY_ALIAS = "holon_debug_session_key"
private const val TRANSFORMATION = "AES/GCM/NoPadding"

internal fun createSessionStore(context: Context): SessionCredentialStore =
    EncryptedDebugSessionStore(
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE),
    )

private class EncryptedDebugSessionStore(
    private val preferences: SharedPreferences,
) : SessionCredentialStore {
    override fun read(): String? {
        val encoded = preferences.getString(SESSION_KEY, null) ?: return null
        return runCatching {
            val parts = encoded.split(':', limit = 2)
            require(parts.size == 2)
            val iv = Base64.decode(parts[0], Base64.NO_WRAP)
            val ciphertext = Base64.decode(parts[1], Base64.NO_WRAP)
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, iv))
            cipher.doFinal(ciphertext).toString(StandardCharsets.UTF_8)
        }.getOrElse {
            clear()
            null
        }
    }

    override fun write(credential: String) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val iv = Base64.encodeToString(cipher.iv, Base64.NO_WRAP)
        val ciphertext =
            Base64.encodeToString(
                cipher.doFinal(credential.toByteArray(StandardCharsets.UTF_8)),
                Base64.NO_WRAP,
            )
        check(preferences.edit().putString(SESSION_KEY, "$iv:$ciphertext").commit()) {
            "Unable to persist the debug session credential"
        }
    }

    override fun clear() {
        check(preferences.edit().remove(SESSION_KEY).commit()) {
            "Unable to clear the debug session credential"
        }
    }

    private fun key(): SecretKey {
        val keyStore = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        val existing = keyStore.getKey(KEY_ALIAS, null)
        if (existing is SecretKey) {
            return existing
        }
        val generator =
            KeyGenerator.getInstance(
                KeyProperties.KEY_ALGORITHM_AES,
                KEYSTORE,
            )
        generator.init(
            KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            ).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .build(),
        )
        return generator.generateKey()
    }
}
