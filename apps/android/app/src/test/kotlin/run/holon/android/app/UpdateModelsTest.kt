package run.holon.android.app

import kotlin.test.assertFalse
import kotlin.test.assertTrue
import org.junit.Test

class UpdateModelsTest {
    @Test
    fun `only successful channel checks advance the throttle and only updates prompt`() {
        val direct = ReleaseUpdateResult.Available(AndroidRelease("0.1.2", 1002, "", "", "app.apk"))
        for (result in listOf(direct, ReleaseUpdateResult.PlayAvailable(1002))) {
            assertTrue(result.hasUpdate)
            assertTrue(result.isSuccessfulCheck)
        }
        assertFalse(ReleaseUpdateResult.NoUpdate.hasUpdate)
        assertTrue(ReleaseUpdateResult.NoUpdate.isSuccessfulCheck)
        for (result in listOf(ReleaseUpdateResult.Unavailable("offline"), ReleaseUpdateResult.InvalidRelease("invalid"))) {
            assertFalse(result.hasUpdate)
            assertFalse(result.isSuccessfulCheck)
        }
        assertFalse(AndroidUpdateUiState().showPrompt)
        assertTrue(AndroidUpdateUiState(result = direct).actionRequest == null)
    }
}
