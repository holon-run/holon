package run.holon.android.app

import android.content.Context
import android.content.SharedPreferences
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Base64
import java.nio.charset.StandardCharsets
import java.security.InvalidAlgorithmParameterException
import java.security.InvalidKeyException
import java.security.KeyStore
import javax.crypto.AEADBadTagException
import javax.crypto.BadPaddingException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import run.holon.android.sdk.ProfileSessionCredentialStore

private const val PREFS = "holon_debug_session"
private const val SESSION_KEY = "session_credential"
private const val KEYSTORE = "AndroidKeyStore"
private const val KEY_ALIAS = "holon_debug_session_key"
private const val TRANSFORMATION = "AES/GCM/NoPadding"

/** Stores only the revocable native session; the exchange token never reaches disk. */
internal fun createSessionStore(context: Context): ProfileSessionCredentialStore =
    EncryptedSessionStore(context.getSharedPreferences(PREFS, Context.MODE_PRIVATE))

internal interface LegacySessionCredentialMigrator {
    fun migrateLegacy(profileId: String)
}

private class EncryptedSessionStore(
    private val preferences: SharedPreferences,
) : ProfileSessionCredentialStore, LegacySessionCredentialMigrator {
    private val lock = Any()
    private val cachedCredentials = mutableMapOf<String, String>()

    override fun read(): String? = readEncoded(SESSION_KEY)

    override fun read(profileId: String): String? = readEncoded(profileKey(profileId))

    override fun migrateLegacy(profileId: String) {
        val legacy = read() ?: return
        if (readEncoded(profileKey(profileId)) == null) {
            write(profileId, legacy)
        }
        clear()
    }

    private fun readEncoded(keyName: String): String? {
        synchronized(lock) {
            cachedCredentials[keyName]?.let { return it }
            val encoded = preferences.getString(keyName, null) ?: return null
            return try {
                val parts = encoded.split(':', limit = 2)
                require(parts.size == 2) { "Malformed encrypted session credential" }
                val cipher = Cipher.getInstance(TRANSFORMATION)
                cipher.init(
                    Cipher.DECRYPT_MODE,
                    key(),
                    GCMParameterSpec(128, Base64.decode(parts[0], Base64.NO_WRAP)),
                )
                cipher.doFinal(Base64.decode(parts[1], Base64.NO_WRAP))
                    .toString(StandardCharsets.UTF_8)
                    .also { cachedCredentials[keyName] = it }
            } catch (error: Throwable) {
                if (!isPermanentCredentialFailure(error)) throw error
                check(preferences.edit().remove(keyName).commit()) {
                    "Unable to clear the invalid native session credential"
                }
                cachedCredentials.remove(keyName)
                null
            }
        }
    }

    override fun write(credential: String) = writeEncoded(SESSION_KEY, credential)

    override fun write(profileId: String, credential: String) =
        writeEncoded(profileKey(profileId), credential)

    private fun writeEncoded(keyName: String, credential: String) {
        synchronized(lock) {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.ENCRYPT_MODE, key())
            val iv = Base64.encodeToString(cipher.iv, Base64.NO_WRAP)
            val encrypted =
                Base64.encodeToString(
                    cipher.doFinal(credential.toByteArray(StandardCharsets.UTF_8)),
                    Base64.NO_WRAP,
                )
            check(preferences.edit().putString(keyName, "$iv:$encrypted").commit()) {
                "Unable to persist the native session credential"
            }
            cachedCredentials[keyName] = credential
        }
    }

    override fun clear() {
        synchronized(lock) {
            check(preferences.edit().remove(SESSION_KEY).commit()) {
                "Unable to clear the native session credential"
            }
            cachedCredentials.remove(SESSION_KEY)
        }
    }

    override fun clear(profileId: String) {
        synchronized(lock) {
            val keyName = profileKey(profileId)
            check(preferences.edit().remove(keyName).commit()) {
                "Unable to clear the native session credential"
            }
            cachedCredentials.remove(keyName)
        }
    }

    private fun isPermanentCredentialFailure(error: Throwable): Boolean =
        error is AEADBadTagException ||
            error is BadPaddingException ||
            error is IllegalArgumentException ||
            error is InvalidAlgorithmParameterException ||
            error is InvalidKeyException ||
            error is KeyPermanentlyInvalidatedException

    private fun key(): SecretKey {
        val keyStore = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        (keyStore.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE).run {
            init(
                KeyGenParameterSpec.Builder(
                    KEY_ALIAS,
                    KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                ).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .build(),
            )
            generateKey()
        }
    }

    private fun profileKey(profileId: String): String =
        "session_credential_profile_${profileId.replace(Regex("[^A-Za-z0-9_-]"), "_")}"
}
