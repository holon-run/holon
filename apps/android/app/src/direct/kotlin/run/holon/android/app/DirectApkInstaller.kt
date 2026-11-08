package run.holon.android.app

import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.FileProvider
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import okhttp3.Call
import okhttp3.OkHttpClient
import okhttp3.Request
import java.io.Closeable
import java.io.File
import java.io.IOException
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

internal class DirectApkInstaller(private val context: Context) : Closeable {
    private val directory = File(context.cacheDir, "updates")
    private val identifier = UUID.randomUUID().toString()
    private val currentPartial = AtomicReference<File?>(null)
    private val currentApk = AtomicReference<File?>(null)
    private val closed = AtomicBoolean(false)
    private val activeCall = AtomicReference<Call?>(null)
    private var handedToInstaller = false
    private val client = OkHttpClient.Builder()
        .connectTimeout(15, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
        .callTimeout(180, TimeUnit.SECONDS)
        .followSslRedirects(false)
        .build()

    suspend fun download(release: AndroidRelease) = withContext(Dispatchers.IO) {
        val job = currentCoroutineContext()
        val attempt = UUID.randomUUID().toString()
        val partial = File(directory, "$identifier-$attempt.partial")
        val apk = File(directory, "$identifier-$attempt.apk")
        var call: Call? = null
        var complete = false
        try {
            checkOpen()
            clear()
            currentPartial.set(partial)
            updateIntegrityProblem(release.apkSize, release.apkSha256)?.let { throw IOException(it) }
            if (!isSecureUpdateUrl(release.apkUrl)) throw IOException("APK download requires a valid HTTPS URL.")
            if (!directory.isDirectory && !directory.mkdirs()) throw IOException("Cannot create the update cache.")
            // Retain at most one recent APK for an external installer after a configuration change.
            val oldFiles = directory.listFiles().orEmpty().filter { it.extension in setOf("apk", "partial") }
            val retained = oldFiles.filter { it.extension == "apk" }.maxByOrNull { it.lastModified() }
                ?.takeIf { it.lastModified() >= System.currentTimeMillis() - TimeUnit.DAYS.toMillis(1) }
            oldFiles.filter { it != retained && it != partial }.forEach { it.delete() }
            call = client.newCall(Request.Builder().url(release.apkUrl).build())
            activeCall.set(call)
            if (closed.get()) call.cancel()
            call.readCancellable { response, checkActive ->
                if (!response.isSuccessful) throw IOException("APK download returned HTTP ${response.code}.")
                val body = response.body ?: throw IOException("The APK download is empty.")
                if (body.contentLength() >= 0 && body.contentLength() != release.apkSize) {
                    throw IOException("The APK server length differs from the published size.")
                }
                body.byteStream().use {
                    writeVerifiedApk(it, partial, release.apkSize, release.apkSha256) {
                        checkActive()
                        checkOpen()
                    }
                }
            }
            job.ensureActive()
            checkOpen()
            validate(partial, release) { job.ensureActive(); checkOpen() }
            job.ensureActive()
            if (!partial.renameTo(apk)) throw IOException("Cannot save the verified APK.")
            currentApk.getAndSet(apk)?.delete()
            complete = true
        } finally {
            activeCall.compareAndSet(call, null)
            currentPartial.compareAndSet(partial, null)
            partial.delete()
            if (!complete || closed.get() || !job.isActive) {
                currentApk.compareAndSet(apk, null)
                apk.delete()
            }
        }
    }

    suspend fun validateForInstall(release: AndroidRelease) = withContext(Dispatchers.IO) {
        val job = currentCoroutineContext()
        val apk = currentApk.get() ?: throw IOException("The downloaded APK is no longer available. Download it again.")
        try {
            checkOpen()
            validate(apk, release) { job.ensureActive(); checkOpen() }
        } catch (error: Exception) {
            apk.delete()
            throw error
        }
    }

    @Suppress("DEPRECATION")
    fun installationIntent(): Intent {
        val apk = currentApk.get() ?: throw IOException("The downloaded APK is no longer available.")
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", apk)
        return Intent(Intent.ACTION_INSTALL_PACKAGE).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            clipData = ClipData.newRawUri("Holon update", uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            putExtra(Intent.EXTRA_RETURN_RESULT, true)
        }
    }

    fun markHandedToInstaller() { handedToInstaller = true }

    fun clear() {
        handedToInstaller = false
        currentPartial.getAndSet(null)?.delete()
        currentApk.getAndSet(null)?.delete()
    }

    override fun close() {
        closed.set(true)
        activeCall.getAndSet(null)?.cancel()
        currentPartial.getAndSet(null)?.delete()
        if (!handedToInstaller) currentApk.getAndSet(null)?.delete()
    }

    private fun checkOpen() {
        if (closed.get()) throw kotlinx.coroutines.CancellationException("Update screen closed")
    }

    @Suppress("DEPRECATION")
    private fun validate(file: File, release: AndroidRelease, checkActive: () -> Unit) {
        verifyApkFile(file, release.apkSize, release.apkSha256, checkActive)
        val flags = if (Build.VERSION.SDK_INT >= 28) PackageManager.GET_SIGNING_CERTIFICATES else PackageManager.GET_SIGNATURES
        val manager = context.packageManager
        val archive = manager.getPackageArchiveInfo(file.absolutePath, flags)
            ?: throw IOException("Android could not read the downloaded APK.")
        val installed = manager.getPackageInfo(context.packageName, flags)
        apkInstallProblem(installed.identity(), archive.identity(), release.versionCode)?.let { throw IOException(it) }
        checkActive()
    }

    @Suppress("DEPRECATION")
    private fun PackageInfo.identity(): UpdateApkIdentity {
        val certificates = if (Build.VERSION.SDK_INT >= 28) signingInfo?.apkContentsSigners else signatures
        // Require the current signer set exactly; unverified key-rotation capabilities fail closed.
        val signers = certificates.orEmpty().map { signature ->
            MessageDigest.getInstance("SHA-256").digest(signature.toByteArray())
                .joinToString("") { "%02x".format(it) }
        }.toSet()
        val code = if (Build.VERSION.SDK_INT >= 28) longVersionCode else versionCode.toLong()
        return UpdateApkIdentity(packageName, code, signers)
    }
}
