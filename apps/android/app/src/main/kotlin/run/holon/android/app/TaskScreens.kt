package run.holon.android.app

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import run.holon.android.sdk.HolonTaskSnapshot

private fun taskLabel(status: String): String = ui(when (status) {
    "queued" -> "排队中"
    "running" -> "运行中"
    "cancelling" -> "正在取消"
    "completed" -> "已完成"
    "failed" -> "失败"
    "cancelled" -> "已取消"
    "interrupted" -> "已中断"
    else -> "状态未知"
})

@Composable
internal fun TaskRow(task: HolonTaskSnapshot, onClick: () -> Unit) {
    Column(Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(task.summary ?: task.command ?: task.taskId, modifier = Modifier.weight(1f), maxLines = 2, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium)
            Spacer(Modifier.width(8.dp))
            Text(taskLabel(task.status), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Text(listOfNotNull(task.kind, task.childAgentId).joinToString(" · "), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
internal fun TaskDetailScreen(state: HolonUiState, viewModel: WorkActions) {
    val task = state.selectedTask ?: return
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = viewModel::closeTask) { Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = ui("返回上一级")) }
                Text(ui("任务详情"), style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                IconButton(onClick = viewModel::refreshTasks, enabled = !state.tasksBusy && state.online) { Icon(Icons.Default.Refresh, contentDescription = ui("刷新任务")) }
            }
            if (state.tasksBusy) LinearProgressIndicator(Modifier.fillMaxWidth())
        }
        item { Text(task.summary ?: task.taskId, style = MaterialTheme.typography.titleMedium) }
        item { Text(taskLabel(task.status) + " · " + task.kind, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        task.progress?.let { progress -> item { MarkdownText(progress) } }
        task.childAgentId?.let { child -> item { Text(ui("子 Agent") + ": $child") } }
        task.command?.let { command -> item { SelectionContainer { Text(command, fontFamily = FontFamily.Monospace) } } }
        item { SelectionContainer { Text(task.taskId, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) } }
        state.tasksError?.let { error -> item { Text(ui(error), color = MaterialTheme.colorScheme.error) } }
        item {
            OutlinedButton(onClick = viewModel::loadTaskOutput, enabled = !state.tasksBusy && state.online) {
                Text(ui(if (state.taskOutput == null) "查看任务输出" else "刷新任务输出"))
            }
        }
        state.taskOutput?.let { output ->
            output.resultSummary?.takeIf(String::isNotBlank)?.let { summary -> item { MarkdownText(summary) } }
            output.outputPreview?.takeIf(String::isNotBlank)?.let { text -> item { SelectionContainer { Text(text, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall) } } }
            if (output.truncated) item { Text(ui("任务输出为截断预览"), color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodySmall) }
            if (output.outputPreview.isNullOrBlank() && output.resultSummary.isNullOrBlank()) item { Text(ui("暂无任务输出")) }
        }
    }
}
