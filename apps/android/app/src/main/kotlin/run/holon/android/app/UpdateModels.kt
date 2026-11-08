package run.holon.android.app

internal data class AndroidRelease(
    val version: String,
    val versionCode: Long,
    val releaseUrl: String,
    val releaseNotes: String,
    val apkName: String,
    val apkUrl: String = "",
    val apkSha256: String = "",
    val apkSize: Long = 0,
)

internal sealed interface ReleaseUpdateResult {
    data class Available(val release: AndroidRelease) : ReleaseUpdateResult
    data class PlayAvailable(val versionCode: Int) : ReleaseUpdateResult
    data object NoUpdate : ReleaseUpdateResult
    data class Unavailable(val reason: String) : ReleaseUpdateResult
    data class InvalidRelease(val reason: String) : ReleaseUpdateResult
}

internal val ReleaseUpdateResult.hasUpdate: Boolean
    get() = this is ReleaseUpdateResult.Available || this is ReleaseUpdateResult.PlayAvailable

internal val ReleaseUpdateResult.isSuccessfulCheck: Boolean
    get() = hasUpdate || this is ReleaseUpdateResult.NoUpdate

internal data class AndroidUpdateUiState(
    val isChecking: Boolean = false,
    val result: ReleaseUpdateResult? = null,
    val lastCheckedAt: Long? = null,
    val showPrompt: Boolean = false,
    // A user gesture, consumed by the Activity-owned channel host. Never auto-install on check.
    val actionRequest: ReleaseUpdateResult? = null,
)
