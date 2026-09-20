package run.holon.android.sdk

/**
 * Platform-provided storage for the revocable credential returned by session exchange.
 *
 * Implementations should use Android secure storage. The SDK deliberately does not
 * choose a storage backend or persist credentials by itself.
 */
public interface SessionCredentialStore {
    public fun read(): String?

    public fun write(credential: String)

    public fun clear()
}
