package run.holon.android.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import app.cash.paparazzi.DeviceConfig
import app.cash.paparazzi.Paparazzi
import com.android.resources.Density
import com.android.resources.ScreenOrientation
import java.util.TimeZone
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.junit.runners.Parameterized
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonJsonDocument
import run.holon.android.sdk.HolonMessageBatch
import run.holon.android.sdk.HolonTaskResultPresentation
import run.holon.android.sdk.HolonTurnInput

@RunWith(Parameterized::class)
class ConversationPresentationVisualTest(
    private val width: Int,
    private val height: Int,
    private val fontScale: Float,
    private val language: String,
) {
    companion object {
        @JvmStatic @Parameterized.Parameters(name = "{0}x{1}-font{2}-{3}")
        fun devices() = listOf(
            arrayOf<Any>(320, 800, 1f, "zh"),
            arrayOf<Any>(360, 800, 1f, "en"),
            arrayOf<Any>(320, 800, 2f, "en"),
            arrayOf<Any>(640, 360, 1f, "zh"),
        )
    }

    @get:Rule val paparazzi = Paparazzi(
        deviceConfig = DeviceConfig.PIXEL_5.copy(
            screenWidth = width, screenHeight = height, xdpi = 160, ydpi = 160,
            density = Density.MEDIUM, locale = language,
            orientation = if (width > height) ScreenOrientation.LANDSCAPE else ScreenOrientation.PORTRAIT,
        ),
        theme = "android:style/Theme.Material.Light.NoActionBar",
    )
    private val timeZone = TimeZone.getDefault()
    @Before fun configure() {
        UiCopy.initialize(paparazzi.context)
        UiCopy.select(paparazzi.context, language)
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
    }
    @After fun restore() {
        UiCopy.select(paparazzi.context, null)
        TimeZone.setDefault(timeZone)
    }

    private val operator = HolonConversationTurn(
        "operator", "", "operator", listOf(HolonTurnInput("input", "请检查构建结果并说明下一步。", "Operator", "operator")),
        "terminal", "completed", "available", null, listOf("brief"),
        "2026-10-20T10:32:00Z", "2026-10-20T10:36:39Z", true, buildJsonObject {}, turnIndex = 127, durationMillis = 279000,
    )
    private val taskInput = HolonTurnInput("task-input", "command failed", null, "task",
        taskResult = HolonTaskResultPresentation("task", "failed", "Build project", "Missing dependency", runtimeOnly = true))
    private val taskTurn = operator.copy(id = "task", presentationClass = "task", inputs = listOf(taskInput), turnIndex = 128)

    @Test fun resultsAndTaskDetailsHaveSeparateHierarchy() {
        paparazzi.snapshot {
            CompositionLocalProvider(LocalDensity provides androidx.compose.ui.unit.Density(LocalDensity.current.density, fontScale)) {
                MaterialTheme {
                    Surface {
                        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(12.dp),
                            verticalArrangement = Arrangement.spacedBy(10.dp)) {
                            item { TurnBoundaryHeader(operator, false, onSource = {}) {} }
                            item { OperatorInput(operator) }
                            item { MarkdownText("**检查结论**\n\n构建缺少依赖，尚未修复。请确认是否继续安装；命令输出保留在执行详情。") }
                            item { TurnBoundaryHeader(taskTurn, false, onSource = {}) {} }
                            item { MarkdownText("本轮分析正常结束，不代表上一个命令成功。") }
                            item { TurnBoundaryHeader(operator.copy(id = "quiet", presentationClass = "system",
                                inputs = emptyList(), briefIds = emptyList(), resultKind = "none", turnIndex = 129), false) {} }
                            item { TurnBoundaryHeader(taskTurn.copy(id = "active", executionKind = "active", terminalOutcome = null,
                                completedAt = null, settled = false, resultKind = "pending", briefIds = emptyList(), turnIndex = 130), true, onSource = {}) {} }
                            item { TaskResultProcessRow(taskInput, taskTurn.startedAt) {} }
                        }
                    }
                }
            }
        }
    }

    @Test fun localReceiptShowsOriginalDateInsteadOfAppearingNew() {
        val old = OutboxEntity(
            "old-request", "scope", "fixture", "This message was sent earlier.", "[]",
            "received", "old-message", null,
            java.time.Instant.parse("2025-10-01T23:59:00Z").toEpochMilli(),
            java.time.Instant.parse("2026-10-22T03:00:00Z").toEpochMilli(),
        )
        paparazzi.snapshot {
            CompositionLocalProvider(LocalDensity provides androidx.compose.ui.unit.Density(LocalDensity.current.density, fontScale)) {
                MaterialTheme { Surface { LocalMessageCard(old, false, {}, {}, {}) } }
            }
        }
    }

    @Test fun canonicalSourceRetainsExternalIdentity() {
        val message = HolonMessageBatch.from(HolonJsonDocument(Json.parseToJsonElement("""
            {"messages":[{"id":"input","agent_id":"fixture","created_at":"2026-10-20T10:32:00Z",
            "kind":"channel_event","priority":"normal","origin":{"kind":"channel","channel_id":"fixture","sender_id":"external-sender"},
            "authority_class":"external_evidence","body":{"type":"text","text":"This is the full canonical body. The turn icon does not change this message's authority."}}]}
        """.trimIndent()))).messages.single()
        val source = SourceInspection(SourceInspectionScope(null, "fixture", 1, null, null), "operator",
            operator.inputs.single(), 1, loading = false, message = message)
        paparazzi.snapshot {
            CompositionLocalProvider(LocalDensity provides androidx.compose.ui.unit.Density(LocalDensity.current.density, fontScale)) {
                MaterialTheme { Surface { SourceInspectionScreen(source, {}, {}) } }
            }
        }
    }
}
