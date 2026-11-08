package run.holon.android.app

import java.io.ByteArrayInputStream
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.nio.file.Files
import java.security.MessageDigest
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlinx.coroutines.CancellationException

class ApkVerificationTest {
    private val bytes = "verified APK fixture".toByteArray()
    private val hash = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

    @Test
    fun `accepts only exact length and checksum including uppercase hex`() = withDestination { file ->
        writeVerifiedApk(bytes.inputStream(), file, bytes.size.toLong(), hash.uppercase())
        assertContentEquals(bytes, file.readBytes())
        verifyApkFile(file, bytes.size.toLong(), hash)
    }

    @Test
    fun `rejects missing unsupported and oversized integrity metadata`() {
        assertNotNull(updateIntegrityProblem(0, hash))
        assertNotNull(updateIntegrityProblem(-1, hash))
        assertNotNull(updateIntegrityProblem(MAX_UPDATE_APK_BYTES + 1, hash))
        assertNotNull(updateIntegrityProblem(20, "not-a-hash"))
        assertNull(updateIntegrityProblem(MAX_UPDATE_APK_BYTES, hash))
    }

    @Test
    fun `deletes truncated oversized and hash-mismatched partial downloads`() = withDestination { file ->
        for ((size, checksum) in listOf(
            bytes.size.toLong() + 1 to hash,
            bytes.size.toLong() - 1 to hash,
            bytes.size.toLong() to "0".repeat(64),
        )) {
            assertFailsWith<IOException> { writeVerifiedApk(bytes.inputStream(), file, size, checksum) }
            assertFalse(file.exists())
        }
    }

    @Test
    fun `deletes partial file on input error`() = withDestination { file ->
        val input = object : InputStream() {
            var reads = 0
            override fun read(): Int = if (reads++ < 3) 1 else throw IOException("Connection lost")
        }
        assertFailsWith<IOException> { writeVerifiedApk(input, file, 100, hash) }
        assertFalse(file.exists())
    }

    @Test
    fun `deletes partial file when cancelled during streaming`() = withDestination { file ->
        var probes = 0
        assertFailsWith<CancellationException> {
            writeVerifiedApk(ByteArrayInputStream(ByteArray(100_000)), file, 100_000, hash) {
                if (++probes > 2) throw CancellationException("User cancelled")
            }
        }
        assertFalse(file.exists())
    }

    @Test
    fun `rejects cached files modified after download`() = withDestination { file ->
        writeVerifiedApk(bytes.inputStream(), file, bytes.size.toLong(), hash)
        file.writeBytes(ByteArray(bytes.size))
        assertFailsWith<IOException> { verifyApkFile(file, bytes.size.toLong(), hash) }
        file.appendText("extra")
        assertFailsWith<IOException> { verifyApkFile(file, bytes.size.toLong(), hash) }
    }

    @Test
    fun `requires exact package release version and a strict upgrade`() {
        val installed = UpdateApkIdentity("run.holon.android", 100, setOf("official"))
        val archive = installed.copy(versionCode = 101)
        assertNull(apkInstallProblem(installed, archive, 101))
        assertNotNull(apkInstallProblem(installed, archive.copy(packageName = "other.app"), 101))
        assertNotNull(apkInstallProblem(installed, archive, 102))
        assertNotNull(apkInstallProblem(installed, installed, 100))
        assertNotNull(apkInstallProblem(installed, archive.copy(versionCode = 99), 99))
    }

    @Test
    fun `rejects debug unknown and changed signatures without uninstalling`() {
        val installed = UpdateApkIdentity("run.holon.android", 100, setOf("debug"))
        val archive = installed.copy(versionCode = 101, signers = setOf("release"))
        val reason = apkInstallProblem(installed, archive, 101)
        assertNotNull(reason)
        kotlin.test.assertTrue(reason.contains("never uninstall"))
        assertNotNull(apkInstallProblem(installed.copy(signers = emptySet()), archive, 101))
        assertNotNull(apkInstallProblem(installed, archive.copy(signers = emptySet()), 101))
        assertNotNull(apkInstallProblem(installed, archive.copy(signers = setOf("debug", "extra")), 101))
    }

    @Test
    fun `requires HTTPS and disallows credentials in download URLs`() {
        kotlin.test.assertTrue(isSecureUpdateUrl("https://github.com/holon-run/holon/releases/download/v1/a.apk"))
        assertFalse(isSecureUpdateUrl("http://example.test/a.apk"))
        assertFalse(isSecureUpdateUrl("https://user:password@example.test/a.apk"))
        assertFalse(isSecureUpdateUrl("file:///cache/update.apk"))
    }

    private fun withDestination(test: (File) -> Unit) {
        val directory = Files.createTempDirectory("holon-apk-verification").toFile()
        try {
            test(File(directory, "update.partial"))
        } finally {
            directory.deleteRecursively()
        }
    }
}
