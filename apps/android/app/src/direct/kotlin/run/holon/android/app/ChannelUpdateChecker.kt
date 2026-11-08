package run.holon.android.app

import android.content.Context

internal class ChannelUpdateChecker(@Suppress("UNUSED_PARAMETER") context: Context) {
    private val checker = ReleaseUpdateChecker()

    suspend fun check(currentVersionCode: Long): ReleaseUpdateResult = checker.check(currentVersionCode)
}
