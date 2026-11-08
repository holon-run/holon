package run.holon.android.app

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.security.MessageDigest

internal const val MAX_UPDATE_APK_BYTES = 256L * 1024 * 1024

internal fun updateIntegrityProblem(size: Long, sha256: String): String? = when {
    size !in 1..MAX_UPDATE_APK_BYTES ->
        "This release has no supported APK size (maximum 256 MiB)."
    !sha256.matches(Regex("[a-fA-F0-9]{64}")) ->
        "This release has no valid SHA-256 checksum. In-app download is not allowed."
    else -> null
}

internal fun writeVerifiedApk(
    input: InputStream,
    destination: File,
    expectedSize: Long,
    expectedSha256: String,
    checkActive: () -> Unit = {},
) {
    var complete = false
    try {
        updateIntegrityProblem(expectedSize, expectedSha256)?.let { throw IOException(it) }
        checkActive()
        val digest = MessageDigest.getInstance("SHA-256")
        var count = 0L
        destination.outputStream().use { output ->
            val buffer = ByteArray(32 * 1024)
            while (true) {
                checkActive()
                val read = input.read(buffer)
                if (read < 0) break
                count += read
                if (count > expectedSize) throw IOException("APK exceeds the published size.")
                digest.update(buffer, 0, read)
                output.write(buffer, 0, read)
            }
        }
        if (count != expectedSize) throw IOException("APK download is incomplete.")
        val actual = digest.digest().joinToString("") { "%02x".format(it) }
        if (!actual.equals(expectedSha256, ignoreCase = true)) {
            throw IOException("APK SHA-256 does not match the release checksum.")
        }
        checkActive()
        complete = true
    } finally {
        if (!complete) destination.delete()
    }
}

internal fun verifyApkFile(
    file: File,
    expectedSize: Long,
    expectedSha256: String,
    checkActive: () -> Unit = {},
) {
    updateIntegrityProblem(expectedSize, expectedSha256)?.let { throw IOException(it) }
    if (!file.isFile || file.length() != expectedSize) throw IOException("Downloaded APK size changed.")
    val digest = MessageDigest.getInstance("SHA-256")
    file.inputStream().use { input ->
        val buffer = ByteArray(32 * 1024)
        var count = 0L
        while (true) {
            checkActive()
            val read = input.read(buffer)
            if (read < 0) break
            count += read
            if (count > expectedSize) throw IOException("Downloaded APK size changed.")
            digest.update(buffer, 0, read)
        }
        if (count != expectedSize) throw IOException("Downloaded APK size changed.")
    }
    val actual = digest.digest().joinToString("") { "%02x".format(it) }
    if (!actual.equals(expectedSha256, ignoreCase = true)) throw IOException("Downloaded APK checksum changed.")
    checkActive()
}

internal data class UpdateApkIdentity(
    val packageName: String,
    val versionCode: Long,
    val signers: Set<String>,
)

internal fun apkInstallProblem(
    installed: UpdateApkIdentity,
    archive: UpdateApkIdentity,
    expectedVersionCode: Long,
): String? = when {
    archive.packageName != installed.packageName -> "The APK belongs to a different application."
    archive.versionCode != expectedVersionCode -> "The APK version does not match the published release."
    archive.versionCode <= installed.versionCode -> "This APK is not newer than the installed application."
    installed.signers.isEmpty() || archive.signers.isEmpty() -> "The APK signing certificate could not be verified."
    installed.signers != archive.signers ->
        "The APK signature differs from this installation (for example, debug versus release). " +
            "It cannot be updated in place. Holon will never uninstall your existing app or data automatically."
    else -> null
}
