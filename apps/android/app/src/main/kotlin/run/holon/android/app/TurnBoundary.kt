package run.holon.android.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Chat
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.CallSplit
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.Schedule
import androidx.compose.material.icons.filled.SmartToy
import androidx.compose.material.icons.filled.HelpOutline
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import run.holon.android.sdk.HolonConversationTurn

internal fun turnSourceName(presentationClass: String?): String = ui(when (presentationClass) {
    "operator" -> "用户消息"
    "task" -> "任务结果"
    "external" -> "外部消息"
    "timer" -> "定时检查"
    "internal" -> "后台续接"
    "system" -> "系统唤醒"
    "operational" -> "运行时操作"
    else -> "未知来源"
})

private fun turnSourceIcon(presentationClass: String?): ImageVector = when (presentationClass) {
    "operator" -> Icons.Default.Person
    "task", "operational" -> Icons.Default.Build
    "external" -> Icons.AutoMirrored.Filled.Chat
    "timer" -> Icons.Default.Schedule
    "internal" -> Icons.Default.CallSplit
    "system" -> Icons.Default.SmartToy
    else -> Icons.Default.HelpOutline
}

internal fun turnNoticeText(notice: TurnNotice): String = ui(when (notice) {
    TurnNotice.Running -> "执行中"
    TurnNotice.Waiting -> "等待中"
    TurnNotice.Syncing -> "同步中"
    TurnNotice.Failed -> "本轮失败"
    TurnNotice.Interrupted -> "本轮未完成"
    TurnNotice.MissingResult -> "结果缺失"
    TurnNotice.ResultUnavailable -> "结果暂不可用"
    TurnNotice.TruncatedInputs -> "部分输入未载入"
})

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun TurnBoundaryHeader(
    turn: HolonConversationTurn,
    expanded: Boolean,
    onSource: (() -> Unit)? = null,
    onToggle: () -> Unit,
) {
    Column(Modifier.fillMaxWidth().testTag("turn-boundary:${turn.id}")) {
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            val sourceName = turnSourceName(turn.presentationClass)
            if (onSource != null) {
                IconButton(onClick = onSource, modifier = Modifier.size(48.dp).testTag("turn-source:${turn.id}")) {
                    Icon(turnSourceIcon(turn.presentationClass), contentDescription = ui("查看来源") + ": " + sourceName)
                }
            } else {
                Box(Modifier.size(48.dp), contentAlignment = Alignment.Center) {
                    Icon(turnSourceIcon(turn.presentationClass), contentDescription = sourceName)
                }
            }
            Column(Modifier.weight(1f).padding(vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    turn.turnIndex?.let { Text("#$it", style = MaterialTheme.typography.labelSmall) }
                    turn.startedAt?.let { Text(localTimestamp(it), style = MaterialTheme.typography.labelSmall) }
                    compactTurnDuration(turn.durationMillis)?.let { duration ->
                        Text(duration, style = MaterialTheme.typography.labelSmall,
                            modifier = Modifier.semantics { contentDescription = ui("耗时") + " " + duration })
                    }
                }
                turnNotices(turn).forEach { notice ->
                    Text(turnNoticeText(notice), style = MaterialTheme.typography.labelSmall,
                        color = if (notice == TurnNotice.Failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            IconButton(
                onClick = onToggle,
                modifier = Modifier.size(48.dp).testTag("turn-disclosure:${turn.id}").semantics {
                    stateDescription = ui(if (expanded) "已展开" else "已折叠")
                },
            ) {
                Icon(if (expanded) Icons.Default.ExpandMore else Icons.Default.ChevronRight,
                    contentDescription = ui(if (expanded) "收起执行详情" else "展开执行详情"))
            }
        }
    }
}
