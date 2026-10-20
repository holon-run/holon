package run.holon.android.app

import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertHasNoClickAction
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertWidthIsAtLeast
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToIndex
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonJsonDocument
import run.holon.android.sdk.HolonMessageBatch
import run.holon.android.sdk.HolonTurnInput

@RunWith(AndroidJUnit4::class)
class ConversationPresentationUiTest {
    @get:Rule val compose = createEmptyComposeRule()
    private val turn = HolonConversationTurn(
        "turn", "", "operator", listOf(HolonTurnInput("source", "Preview only", null, "operator")),
        "terminal", "completed", "none", null, emptyList(), "2026-10-20T12:00:00Z",
        "2026-10-20T12:04:39Z", true, buildJsonObject {}, turnIndex = 127, durationMillis = 279000,
    )

    @Test fun sourceAndDisclosureAreIndependentAccessibleControlsAtLargeFont() {
        var sourceReads = 0
        var toggles = 0
        withContent({
            CompositionLocalProvider(LocalDensity provides Density(LocalDensity.current.density, 2f)) {
                Box(Modifier.width(320.dp)) {
                    var expanded by remember { mutableStateOf(false) }
                    TurnBoundaryHeader(turn, expanded, onSource = { sourceReads++ }) { expanded = !expanded; toggles++ }
                }
            }
        }) {
            compose.onNodeWithTag("turn-source:turn").assertWidthIsAtLeast(48.dp).assertHeightIsAtLeast(48.dp).performClick()
            compose.onNodeWithTag("turn-disclosure:turn")
                .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, ui("已折叠")))
                .assertWidthIsAtLeast(48.dp).assertHeightIsAtLeast(48.dp).performClick()
            compose.onNodeWithTag("turn-disclosure:turn")
                .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, ui("已展开")))
            assertEquals(1, sourceReads)
            assertEquals(1, toggles)
            compose.onNodeWithText("#127").assertExists()
            compose.onNodeWithText("4m39s").assertExists()
        }
    }

    @Test fun sevenSourcesUseStructuredClassAndUnknownSourceIsNotAnEmptyButton() {
        val classes = listOf("operator", "task", "external", "timer", "internal", "system", "operational")
        withContent({
            Column {
                classes.forEach { kind ->
                    TurnBoundaryHeader(turn.copy(id = kind, presentationClass = kind), false, onSource = {}) {}
                }
                TurnBoundaryHeader(turn.copy(id = "unknown", presentationClass = "future", inputs = emptyList()), false) {}
            }
        }) {
            classes.forEach { kind ->
                compose.onNodeWithContentDescription(ui("查看来源") + ": " + turnSourceName(kind)).assertHasClickAction()
            }
            compose.onNodeWithContentDescription(turnSourceName("future")).assertHasNoClickAction()
        }
    }

    @Test fun offlineSourceLabelsPreviewHonestlyAndSupportsRetryAndBack() {
        var retries = 0
        var backs = 0
        val source = SourceInspection(SourceInspectionScope(null, "fixture", null, null, null), turn.id,
            turn.inputs.single(), 1, loading = false, failure = SourceFailure.Offline)
        withContent({
            SourceInspectionScreen(source, onBack = { backs++ }, onRetry = { retries++ })
        }) {
            compose.onNodeWithText(ui("预览（非完整来源）")).assertExists()
            compose.onNodeWithText("Preview only").assertExists()
            compose.onNodeWithText(ui("消息正文")).assertDoesNotExist()
            compose.onNodeWithText(ui("重试")).performClick()
            compose.onNodeWithText(ui("返回会话")).performClick()
            assertEquals(1, retries)
            assertEquals(1, backs)
        }
    }

    @Test fun longCanonicalBodyAndRawRemainScrollableAtLargeFont() {
        var backs = 0
        val body = "Full canonical source body. ".repeat(120)
        val message = HolonMessageBatch.from(HolonJsonDocument(Json.parseToJsonElement("""
            {"messages":[{"id":"source","agent_id":"fixture","created_at":"2026-10-20T12:00:00Z",
            "kind":"channel_event","priority":"normal","origin":{"kind":"channel","sender_id":"external-sender"},
            "authority_class":"external_evidence","body":{"type":"text","text":"$body"}}]}
        """.trimIndent()))).messages.single()
        val source = SourceInspection(SourceInspectionScope(null, "fixture", null, null, null), turn.id,
            turn.inputs.single(), 1, loading = false, message = message)
        withContent({
            CompositionLocalProvider(LocalDensity provides Density(LocalDensity.current.density, 2f)) {
                Box(Modifier.width(320.dp)) { SourceInspectionScreen(source, onBack = { backs++ }, onRetry = {}) }
            }
        }) {
            compose.onNodeWithText(body).assertExists()
            compose.onNodeWithTag("source-inspection")
                .performScrollToNode(hasText(ui("查看结构化信息")))
            compose.onNodeWithText(ui("查看结构化信息")).performClick()
            compose.onNodeWithTag("source-inspection")
                .performScrollToNode(hasText("\"authority_class\"", substring = true))
            compose.onNodeWithText("\"authority_class\": \"external_evidence\"", substring = true).assertExists()
            compose.onNodeWithTag("source-inspection").performScrollToIndex(0)
            compose.onNodeWithText(ui("返回会话")).performClick()
            assertEquals(1, backs)
        }
    }

    private fun withContent(content: @Composable () -> Unit, test: () -> Unit) {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val language = UiCopy.preference()
        UiCopy.select(context, "en")
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                scenario.onActivity { activity -> activity.setContent { MaterialTheme { content() } } }
                compose.waitForIdle()
                test()
            }
        } finally {
            UiCopy.select(context, language)
        }
    }
}
