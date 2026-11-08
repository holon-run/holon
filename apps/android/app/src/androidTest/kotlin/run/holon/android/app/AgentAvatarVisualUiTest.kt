package run.holon.android.app

import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertHeightIsAtLeast
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertWidthIsAtLeast
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import run.holon.android.sdk.AgentSummary

@RunWith(AndroidJUnit4::class)
class AgentAvatarVisualUiTest {
    @get:Rule val compose = createEmptyComposeRule()

    @Test fun modelAttachmentSendAndStopRemainAccessibleOnNarrowScreenAtDoubleFont() {
        var models = 0
        var files = 0
        var sends = 0
        var stops = 0
        withContent({
            MessageComposer(
                "Review these changes.", emptyList(), false, false, true, false,
                {}, { sends++ }, { stops++ }, {}, {}, { files++ }, {},
                modelLabel = "provider@endpoint/a-very-long-model-with-reasoning",
                onModel = { models++ },
            )
        }) {
            compose.onNodeWithContentDescription(ui("选择模型"))
                .assertIsDisplayed().assertWidthIsAtLeast(48.dp).assertHeightIsAtLeast(48.dp).performClick()
            assertEquals(1, models)
            compose.onNodeWithContentDescription(ui("发送"))
                .assertIsEnabled().assertWidthIsAtLeast(48.dp).assertHeightIsAtLeast(48.dp).performClick()
            assertEquals(1, sends)
            compose.onNodeWithContentDescription(ui("添加附件"))
                .assertWidthIsAtLeast(48.dp).assertHeightIsAtLeast(48.dp).performClick()
            compose.onNodeWithText(ui("选择文件")).performClick()
            assertEquals(1, files)
            compose.onNodeWithText(ui("停止本轮")).performClick()
            assertEquals(0, stops)
            compose.onNodeWithText(ui("取消")).performClick()
            assertEquals(0, stops)
            compose.onNodeWithText(ui("停止本轮")).performClick()
            compose.onNodeWithText(ui("停止本轮？")).assertIsDisplayed()
        }
    }

    @Test fun oldDaemonFallbackKeepsNameStatusAndUnreadReadableWithoutAvatarSemantics() {
        var opened = 0
        withContent({
            AgentConversationRow(
                AgentSummary("android", "Android · Release readiness", false, "ready", "idle",
                    "model", pending = 0, currentRunId = null, schedulingPosture = "waiting_for_operator"),
                unreadCount = 2, live = false, onClick = { opened++ },
            )
        }) {
            compose.onNodeWithText("Android · Release readiness").assertIsDisplayed()
            compose.onNodeWithText(ui("等你回应")).assertIsDisplayed()
            compose.onNodeWithText(ui("2 个未读结果")).assertIsDisplayed()
            compose.onNodeWithText("Android · Release readiness").performClick()
            assertEquals(1, opened)
        }
    }

    private fun withContent(content: @Composable () -> Unit, test: () -> Unit) {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val language = UiCopy.preference()
        UiCopy.select(context, "en")
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                scenario.onActivity { activity ->
                    activity.setContent {
                        CompositionLocalProvider(LocalDensity provides Density(LocalDensity.current.density, 2f)) {
                            HolonTheme {
                                Box(Modifier.width(320.dp)) { content() }
                            }
                        }
                    }
                }
                compose.waitForIdle()
                test()
            }
        } finally {
            UiCopy.select(context, language)
        }
    }
}
