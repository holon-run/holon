package run.holon.android.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import app.cash.paparazzi.DeviceConfig
import app.cash.paparazzi.Paparazzi
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Rule
import org.junit.Before
import org.junit.After
import java.util.TimeZone
import org.junit.Test
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonLatestBrief
import run.holon.android.sdk.HolonToolExecutionSnapshot
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonBriefAttachment

class HolonVisualSnapshotTest {
    @Test fun fileFiltersAndActiveTasks() {
        paparazzi.snapshot {
            PreviewFrame {
                Text("文件", style = MaterialTheme.typography.titleLarge)
                FileFilterBar("", {}, false, {}, false, {})
                FileFilterBar("report", {}, true, {}, true, {})
                Text("进行中的任务", style = MaterialTheme.typography.titleMedium)
                TaskRow(run.holon.android.sdk.HolonTaskSnapshot("task", "running", "运行 Android 回归测试", buildJsonObject {}, kind = "command", command = "./gradlew test")) {}
                TaskRow(run.holon.android.sdk.HolonTaskSnapshot("child-task", "queued", "检查文件浏览体验", buildJsonObject {}, kind = "child_agent", childAgentId = "holon-tester")) {}
            }
        }
    }

    @Test fun operatorInputBeforeBrief() {
        paparazzi.snapshot {
            PreviewFrame {
                AgentConversationRow(sampleAgents().last().copy(latestBrief = null), operatorPreview = OperatorPreview("请检查报告并给出改进建议。", null), onClick = {})
            }
        }
    }
    private val originalTimeZone = TimeZone.getDefault()
    @get:Rule
    val paparazzi =
        Paparazzi(
            deviceConfig = DeviceConfig.PIXEL_5.copy(locale = "zh"),
            theme = "android:style/Theme.Material.Light.NoActionBar",
        )

    @Before fun setChineseLocale() {
        UiCopy.initialize(paparazzi.context)
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
    }

    @After fun restoreSettings() {
        TimeZone.setDefault(originalTimeZone)
        UiCopy.select(paparazzi.context, null)
    }

    @Test
    fun conversationResultAndCompactComposer() {
        paparazzi.snapshot {
            PreviewFrame {
                BriefContent(HolonBrief("brief", "tester", null, "result", "2026-09-29T07:12:00Z", "## 验收结果\n\n已完成 **消息接收** 和文件浏览。\n\n- 结果直接展示\n- 过程按需展开", listOf(HolonBriefAttachment("file", "android-acceptance.md", "workspace://test/report.md", null)), null), onFile = { _, _ -> }, onWork = {})
                MessageComposer("", emptyList(), false, false, false, false, {}, {}, {}, {}, {}, {}, {})
            }
        }
    }

    @Test
    fun composerAndRetryEnglishDarkLargeText() {
        UiCopy.select(paparazzi.context, "en")
        paparazzi.snapshot {
            CompositionLocalProvider(LocalDensity provides Density(LocalDensity.current.density, 1.5f)) {
                PreviewFrame(darkTheme = true) {
                    BriefPlaceholder(BriefLoadState.Failed("Connection interrupted"), offline = true, onRetry = {})
                    MessageComposer("Please review the report.\nKeep the draft while viewing files.", emptyList(), false, false, true, false, {}, {}, {}, {}, {}, {}, {})
                }
            }
        }
        UiCopy.select(paparazzi.context, null)
    }

    @Test
    fun workInboxLight() {
        paparazzi.snapshot {
            PreviewFrame {
                Text("Agent 工作", style = MaterialTheme.typography.headlineSmall)
                Text("需要回应的工作排在前面", color = MaterialTheme.colorScheme.onSurfaceVariant)
                sampleAgents().forEach { agent ->
                    AgentConversationRow(agent = agent, onClick = {})
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                }
            }
        }
    }

    @Test
    fun workInboxDark() {
        paparazzi.snapshot {
            PreviewFrame(darkTheme = true) {
                Text("Agent 工作", style = MaterialTheme.typography.headlineSmall)
                sampleAgents().take(2).forEach { agent ->
                    AgentConversationRow(agent = agent, onClick = {})
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                }
            }
        }
    }

    @Test
    fun briefAndExpandedActivity() {
        val activity =
            HolonConversationActivity(
                id = "tool:tool-42",
                kind = "tool",
                summary = "读取工作区中的验收报告并检查输出文件",
                eventSeq = 42,
                revision = 2,
                raw = buildJsonObject {},
            )
        val tool =
            HolonToolExecutionSnapshot(
                toolExecutionId = "tool-42",
                toolName = "workspace.read_file",
                status = "completed",
                summary = "已读取 128 行，内容完整。",
                artifactCount = 1,
                raw = buildJsonObject {
                    put("path", "reports/android-acceptance.md")
                    put("status", "completed")
                },
            )
        paparazzi.snapshot {
            PreviewFrame {
                Text("结果", style = MaterialTheme.typography.headlineSmall)
                MarkdownText(
                    """
                    ## Android 验收完成

                    已确认消息收发、**brief 优先展示**和文件读取。

                    - 本轮过程可持续跟随
                    - 工具输入与输出可内联展开
                    """.trimIndent(),
                )
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                Text("本轮过程", style = MaterialTheme.typography.titleMedium)
                ActivityRow(activity = activity, expanded = true, detail = tool, onOpen = {})
            }
        }
    }

    @Test
    fun emptyOfflineErrorAndPermissionStates() {
        paparazzi.snapshot {
            PreviewFrame {
                CompactStatus("离线缓存", StatusTone.Warning)
                ErrorBanner("连接中断，正在显示上次同步内容。", onDismiss = null)
                EmptyPage("还没有工作结果", "向 Agent 说明你希望完成的工作。")
                ErrorBanner("没有权限读取这个文件，请检查 workspace 授权。", onDismiss = null)
                CompactStatus("实时更新", StatusTone.Accent)
            }
        }
    }

    @Test
    fun networkSettingsWithOneProfile() {
        paparazzi.snapshot {
            PreviewFrame {
                NetworkSection(
                    profiles = listOf(NetworkProfile("office", "Office", "https://office.example/api/", false)),
                    currentNetworkId = "office",
                    busy = false,
                    switchingNetworkId = null,
                    onSwitch = {},
                    onAdd = {},
                )
            }
        }
    }

    private fun sampleAgents(): List<AgentSummary> =
        listOf(
            agent(
                id = "holon-tester",
                name = "Holon Tester",
                posture = "waiting_for_operator",
                preview = "需要你确认真机上的文件分享结果。",
                waitingReason = "awaiting_operator_input",
                workItemId = "work_android_acceptance",
            ),
            agent(
                id = "release-notes",
                name = "Release Notes",
                posture = "active_turn",
                preview = "正在整理最新改动与验收记录。",
                runId = "run-42",
            ),
            agent(
                id = "research",
                name = "Research",
                posture = "idle",
                preview = "已完成开源 Android 应用界面调研。",
            ),
        )

    private fun agent(
        id: String,
        name: String,
        posture: String,
        preview: String,
        waitingReason: String? = null,
        workItemId: String? = null,
        runId: String? = null,
    ) =
        AgentSummary(
            id = id,
            displayName = name,
            isDefault = false,
            registryStatus = "active",
            runtimeStatus = if (runId != null) "running" else "awake_idle",
            effectiveModel = "test",
            pending = 0,
            currentRunId = runId,
            schedulingPosture = posture,
            waitingReason = waitingReason,
            currentWorkItemId = workItemId,
            latestBrief = HolonLatestBrief("brief-$id", "2025-01-01T08:00:00Z", preview, 0),
        )
}

class HolonLargeTextVisualSnapshotTest {
    @get:Rule
    val paparazzi =
        Paparazzi(
            deviceConfig = DeviceConfig.PIXEL_5.copy(fontScale = 1.3f, locale = "zh"),
            theme = "android:style/Theme.Material.Light.NoActionBar",
        )

    @Before fun setChineseLocale() {
        UiCopy.initialize(paparazzi.context)
    }

    @Test
    fun briefAtLargeFontScale() {
        paparazzi.snapshot {
            PreviewFrame {
                Text("结果", style = MaterialTheme.typography.headlineSmall)
                MarkdownText(
                    """
                    ## 结果清晰可读

                    大字号下仍保持 **brief 优先**，正文、列表和操作状态不会互相遮挡。

                    1. 阅读结果
                    2. 查看本轮过程
                    3. 验收关联产物
                    """.trimIndent(),
                )
                ResultLinkRow("本轮过程", "12 条活动", onClick = {})
                ResultLinkRow("产物与关联工作", "3", onClick = {})
            }
        }
    }
}

@Composable
private fun PreviewFrame(
    darkTheme: Boolean = false,
    content: @Composable () -> Unit,
) {
    HolonTheme(darkTheme = darkTheme) {
        Surface(
            modifier = Modifier.fillMaxSize(),
            color = MaterialTheme.colorScheme.background,
        ) {
            Column(
                modifier = Modifier.padding(PaddingValues(horizontal = 16.dp, vertical = 20.dp)),
                verticalArrangement = Arrangement.spacedBy(12.dp),
                content = { content() },
            )
        }
    }
}
