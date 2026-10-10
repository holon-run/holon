package run.holon.android.app

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import app.cash.paparazzi.DeviceConfig
import app.cash.paparazzi.Paparazzi
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import run.holon.android.sdk.HolonContentReportCategory

class ContentReportVisualSnapshotTest {
    @get:Rule
    val paparazzi = Paparazzi(
        deviceConfig = DeviceConfig.PIXEL_5.copy(screenWidth = 720, screenHeight = 1280, locale = "en"),
        theme = "android:style/Theme.Material.Light.NoActionBar",
    )

    @Before fun setEnglishLocale() {
        UiCopy.initialize(paparazzi.context)
        UiCopy.select(paparazzi.context, "en")
    }

    @After fun restoreLocale() {
        UiCopy.select(paparazzi.context, null)
    }

    @Test fun compactReportKeepsActionsVisible() = snapshot(Modifier.fillMaxSize())

    // The remaining viewport when the IME is open must keep the actions visible.
    @Test fun keyboardSizedViewportKeepsActionsVisible() =
        snapshot(Modifier.fillMaxWidth().height(240.dp))

    private fun snapshot(modifier: Modifier) {
        paparazzi.snapshot {
            MaterialTheme {
                Surface {
                    Box(modifier) {
                        ContentReportForm(
                            state = HolonUiState().copy(
                                reportCategory = HolonContentReportCategory.SPAM_OR_OTHER,
                                reportDescription = "TEST ONLY. Review check. No harmful content.",
                            ),
                            onCategorySelected = {},
                            onDescriptionChanged = {},
                            onDismiss = {},
                            onSubmit = {},
                        )
                    }
                }
            }
        }
    }
}
