package run.holon.android.app

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import app.cash.paparazzi.DeviceConfig
import app.cash.paparazzi.Paparazzi
import com.android.resources.Density
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.junit.runners.Parameterized
import run.holon.android.sdk.AgentSummary

@RunWith(Parameterized::class)
class AgentAvatarVisualTest(
    private val width: Int,
    private val fontScale: Float,
    private val dark: Boolean,
) {
    companion object {
        @JvmStatic @Parameterized.Parameters(name = "{0}dp-font{1}-dark{2}")
        fun devices() = listOf(
            arrayOf<Any>(320, 2f, false),
            arrayOf<Any>(360, 1f, false),
            arrayOf<Any>(412, 1.3f, true),
        )
    }

    @get:Rule val paparazzi = Paparazzi(
        deviceConfig = DeviceConfig.PIXEL_5.copy(
            screenWidth = width, screenHeight = 1000, xdpi = 160, ydpi = 160,
            density = Density.MEDIUM, locale = "en",
        ),
        theme = "android:style/Theme.Material.Light.NoActionBar",
    )

    @Before fun configure() {
        UiCopy.initialize(paparazzi.context)
        UiCopy.select(paparazzi.context, "en")
    }

    @After fun restore() { UiCopy.select(paparazzi.context, null) }

    @Test fun oldDaemonAvatarFallbackAndActionHierarchy() {
        frame {
            for (posture in listOf("waiting_for_operator", "active_turn", "idle")) {
                AgentConversationRow(
                    AgentSummary("agent-$posture", "Android · 发布验收 / Release readiness", false,
                        "ready", "idle", "model", pending = 0, currentRunId = null, schedulingPosture = posture),
                    unreadCount = if (posture == "idle") 2 else 0,
                    onClick = {},
                )
            }
        }
    }

    @Test fun longModelSharesInputActionsAtLargeFont() {
        frame {
            MessageComposer(
                "Review the latest changes.", emptyList(), false, false, true, false,
                {}, {}, {}, {}, {}, {}, {},
                modelLabel = "provider@endpoint/a-very-long-model-name-with-reasoning",
                onModel = {},
            )
        }
    }

    private fun frame(content: @Composable () -> Unit) {
        paparazzi.snapshot {
            CompositionLocalProvider(LocalDensity provides androidx.compose.ui.unit.Density(1f, fontScale)) {
                HolonTheme(darkTheme = dark) {
                    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) { Column { content() } }
                }
            }
        }
    }
}
