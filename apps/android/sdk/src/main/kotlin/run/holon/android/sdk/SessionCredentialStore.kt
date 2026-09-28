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

/**
 * Secure credential storage keyed by the locally stable network profile id.
 *
 * The unkeyed methods remain part of [SessionCredentialStore] for migration
 * compatibility with existing SDK clients and storage implementations.
 */
public interface ProfileSessionCredentialStore : SessionCredentialStore {
    public fun read(profileId: String): String?

    public fun write(profileId: String, credential: String)

    public fun clear(profileId: String)
}
