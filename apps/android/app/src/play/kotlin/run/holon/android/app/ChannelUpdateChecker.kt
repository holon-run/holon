package run.holon.android.app

import android.content.Context
import com.google.android.play.core.appupdate.AppUpdateInfo
import com.google.android.play.core.appupdate.AppUpdateManager
import com.google.android.play.core.appupdate.AppUpdateManagerFactory
import com.google.android.play.core.install.model.UpdateAvailability
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeout
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

internal class ChannelUpdateChecker(context: Context) {
    private val manager = AppUpdateManagerFactory.create(context.applicationContext)

    suspend fun check(currentVersionCode: Long): ReleaseUpdateResult =
        try {
            val info = awaitPlayUpdateInfo(manager)
            playUpdateResult(info.updateAvailability(), info.availableVersionCode(), currentVersionCode)
        } catch (_: TimeoutCancellationException) {
            ReleaseUpdateResult.Unavailable(ui("Google Play 检查超时，可稍后重试或打开商店。"))
        } catch (error: kotlinx.coroutines.CancellationException) {
            throw error
        } catch (_: Exception) {
            ReleaseUpdateResult.Unavailable(ui("无法向 Google Play 检查更新，可打开商店查看。"))
        }
}

internal suspend fun awaitPlayUpdateInfo(manager: AppUpdateManager): AppUpdateInfo =
    withTimeout(20_000) {
        suspendCancellableCoroutine { continuation ->
            manager.appUpdateInfo
                .addOnSuccessListener { if (continuation.isActive) continuation.resume(it) }
                .addOnFailureListener { if (continuation.isActive) continuation.resumeWithException(it) }
        }
    }

internal fun playUpdateResult(availability: Int, availableVersionCode: Int, currentVersionCode: Long): ReleaseUpdateResult =
    if (availability == UpdateAvailability.UNKNOWN) {
        ReleaseUpdateResult.Unavailable(ui("无法向 Google Play 检查更新，可打开商店查看。"))
    } else if ((availability == UpdateAvailability.UPDATE_AVAILABLE ||
            availability == UpdateAvailability.DEVELOPER_TRIGGERED_UPDATE_IN_PROGRESS) &&
        availableVersionCode.toLong() > currentVersionCode
    ) {
        ReleaseUpdateResult.PlayAvailable(availableVersionCode)
    } else {
        ReleaseUpdateResult.NoUpdate
    }

internal enum class PlayUpdateRoute { None, InApp, Store }

internal fun playUpdateRoute(availability: Int, immediateAllowed: Boolean, resumeOnly: Boolean): PlayUpdateRoute =
    when {
        availability == UpdateAvailability.DEVELOPER_TRIGGERED_UPDATE_IN_PROGRESS -> PlayUpdateRoute.InApp
        resumeOnly -> PlayUpdateRoute.None
        availability == UpdateAvailability.UPDATE_AVAILABLE && immediateAllowed -> PlayUpdateRoute.InApp
        else -> PlayUpdateRoute.Store
    }
