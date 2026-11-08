package run.holon.android.app

import com.google.android.play.core.install.model.UpdateAvailability
import kotlin.test.assertEquals
import kotlin.test.assertIs
import org.junit.Test

class PlayUpdatePolicyTest {
    @Test
    fun `only Play available updates with a higher code are offered`() {
        assertEquals(
            ReleaseUpdateResult.PlayAvailable(1002),
            playUpdateResult(UpdateAvailability.UPDATE_AVAILABLE, 1002, 1001),
        )
        assertIs<ReleaseUpdateResult.NoUpdate>(playUpdateResult(UpdateAvailability.UPDATE_AVAILABLE, 1001, 1001))
        assertIs<ReleaseUpdateResult.NoUpdate>(playUpdateResult(UpdateAvailability.UPDATE_NOT_AVAILABLE, 1002, 1001))
        assertIs<ReleaseUpdateResult.Unavailable>(playUpdateResult(UpdateAvailability.UNKNOWN, 1002, 1001))
    }

    @Test
    fun `resume only resumes a user approved immediate update`() {
        assertEquals(PlayUpdateRoute.None, playUpdateRoute(UpdateAvailability.UPDATE_AVAILABLE, true, true))
        assertEquals(PlayUpdateRoute.None, playUpdateRoute(UpdateAvailability.UPDATE_NOT_AVAILABLE, false, true))
        assertEquals(
            PlayUpdateRoute.InApp,
            playUpdateRoute(UpdateAvailability.DEVELOPER_TRIGGERED_UPDATE_IN_PROGRESS, true, true),
        )
    }

    @Test
    fun `user action uses Play flow or store but never an APK`() {
        assertEquals(PlayUpdateRoute.InApp, playUpdateRoute(UpdateAvailability.UPDATE_AVAILABLE, true, false))
        assertEquals(PlayUpdateRoute.Store, playUpdateRoute(UpdateAvailability.UPDATE_AVAILABLE, false, false))
        assertEquals(PlayUpdateRoute.Store, playUpdateRoute(UpdateAvailability.UPDATE_NOT_AVAILABLE, false, false))
    }
}
