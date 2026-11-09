package run.holon.android.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
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
class AgentStatusVisualTest(
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

        // One roster row per fixture kind so every icon/tone pair is rendered.
        private fun roster(): List<AgentSummary> = listOf(
            AgentSummary("needs-input", "Play release reviewer", false, "ready", "idle", "model",
                pending = 0, currentRunId = null, schedulingPosture = "waiting_for_operator"),
            AgentSummary("running", "Release readiness", false, "ready", "idle", "model",
                pending = 2, currentRunId = null, schedulingPosture = "active_turn"),
            AgentSummary("runnable", "Publishing pipeline", false, "ready", "idle", "model",
                pending = 2, currentRunId = null, schedulingPosture = "has_queued_input"),
            AgentSummary("waiting", "Dependency auditor", false, "ready", "idle", "model",
                pending = 0, currentRunId = null, schedulingPosture = "waiting_for_task"),
            AgentSummary("blocked", "Play Console ops", false, "ready", "idle", "model",
                pending = 0, currentRunId = null, schedulingPosture = "blocked"),
            AgentSummary("ready", "Code health steward", false, "ready", "idle", "model",
                pending = 0, currentRunId = null, schedulingPosture = "idle"),
            AgentSummary("unknown", "Offline daemon", false, "ready", "offline", "model",
                pending = 0, currentRunId = null, schedulingPosture = "active_turn"),
            AgentSummary("stopped", "Retired supervisor", false, "stopped", "idle", "model",
                pending = 0, currentRunId = null, schedulingPosture = "active_turn"),
            AgentSummary("deleting", "Sandbox clone", false, "deleting", "idle", "model",
                pending = 0, currentRunId = null, schedulingPosture = "idle"),
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

    @Test fun rosterRowsLeadWithStatusIconAndAuxiliaryText() {
        frame(vertical = 4.dp) {
            for (agent in roster()) {
                AgentConversationRow(agent, unreadCount = 0, onClick = {})
            }
        }
    }

    @Test fun selectedHeaderPairsIconWithToneColoredLabel() {
        frame(vertical = 8.dp) {
            for (kind in AgentStatusKind.entries) {
                val status = AgentStatusPresentation(kind)
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                    modifier = Modifier.padding(horizontal = 16.dp),
                ) {
                    AgentStatusIcon(status)
                    Text(
                        "${kind.key} · ${status.label}",
                        style = MaterialTheme.typography.bodySmall,
                        color = toneColor(status.kind.tone),
                    )
                }
            }
        }
    }

    private fun frame(vertical: androidx.compose.ui.unit.Dp, content: @Composable () -> Unit) {
        paparazzi.snapshot {
            CompositionLocalProvider(LocalDensity provides androidx.compose.ui.unit.Density(1f, fontScale)) {
                HolonTheme(darkTheme = dark) {
                    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                        Column(verticalArrangement = Arrangement.spacedBy(vertical)) { content() }
                    }
                }
            }
        }
    }
}
