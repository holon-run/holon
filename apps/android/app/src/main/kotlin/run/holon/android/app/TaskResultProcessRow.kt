package run.holon.android.app

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import run.holon.android.sdk.HolonTaskSnapshot
import run.holon.android.sdk.HolonTurnInput
import kotlinx.serialization.json.buildJsonObject

/** TaskResult is an input, projected next to activities without changing protocol identity. */
@Composable
internal fun TaskResultProcessRow(input: HolonTurnInput, timestamp: String?, onOpen: (HolonTaskSnapshot) -> Unit) {
    val result = input.taskResult ?: return
    Column(Modifier.fillMaxWidth().padding(vertical = 4.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(result.summary ?: ui("收到任务结果"), style = MaterialTheme.typography.labelMedium)
        Text(listOfNotNull(taskResultStatus(result), timestamp?.let(::localTimestamp)).joinToString(" · "),
            style = MaterialTheme.typography.labelSmall,
            color = if (result.status in setOf("failed", "interrupted")) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)
        if (result.responseMessageId == null && result.preview.isNotBlank()) SelectionContainer {
            Text(taskResultPreview(result.preview), style = MaterialTheme.typography.bodySmall)
        }
        TextButton(onClick = { onOpen(HolonTaskSnapshot(result.taskId, result.status, result.summary, buildJsonObject {})) }) {
            Text(ui(if (result.responseMessageId != null) "查看原回复" else "查看任务输出"))
        }
    }
}
