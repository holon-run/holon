@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import run.holon.android.sdk.AgentSummary

@Composable
internal fun ShareToAgentDialog(state: HolonUiState, viewModel: ShareActions) {
    val share = state.pendingShare ?: return
    val scopeKey = state.session?.scopeKey ?: return
    val directTarget = AgentShareShortcuts.target(scopeKey, state.agents, share.targetShortcutId)
    var selectedId by remember(share.id) { mutableStateOf(directTarget?.id) }
    var search by remember(share.id) { mutableStateOf("") }
    val selected = state.agents.firstOrNull { it.id == selectedId }
    AlertDialog(
        onDismissRequest = viewModel::dismissShare,
        title = { Text(ui(if (share.fromTrace) "发送 Trace 给 Agent" else "分享到 Holon Agent")) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                if (share.targetShortcutId != null && directTarget == null) {
                    Text(ui("原 Agent 已不可用，请重新选择。"), color = MaterialTheme.colorScheme.error)
                }
                OutlinedTextField(
                    value = share.text,
                    onValueChange = viewModel::updateShareText,
                    label = { Text(ui("分享内容")) },
                    minLines = 2,
                    maxLines = 5,
                    modifier = Modifier.fillMaxWidth(),
                )
                share.files.forEach { file ->
                    Text(
                        file.name + (file.mediaType?.let { " · $it" } ?: "") +
                            (file.size?.let { " · ${formatBytes(it)}" } ?: ""),
                        style = MaterialTheme.typography.bodySmall,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                if (selected != null) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(selected.displayName, modifier = Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
                        TextButton(onClick = { selectedId = null }) { Text(ui("更换 Agent")) }
                    }
                } else {
                    if (state.agents.isEmpty()) {
                        Text(ui("暂无可用 Agent，请检查连接后重试。"), color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    OutlinedTextField(
                        value = search,
                        onValueChange = { search = it },
                        label = { Text(ui("搜索 Agent")) },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    LazyColumn(modifier = Modifier.heightIn(max = 240.dp)) {
                        items(state.recentAgents.filter {
                            search.isBlank() || it.displayName.contains(search, ignoreCase = true) ||
                                it.id.contains(search, ignoreCase = true)
                        }, key = AgentSummary::id) { agent ->
                            TextButton(onClick = { selectedId = agent.id }, modifier = Modifier.fillMaxWidth()) {
                                Text(agent.displayName, modifier = Modifier.fillMaxWidth())
                            }
                        }
                    }
                }
                state.shareError?.let { Text(ui(it), color = MaterialTheme.colorScheme.error) }
                Text(ui("确认后将作为新消息发送；现有草稿不受影响。"), style = MaterialTheme.typography.bodySmall)
            }
        },
        confirmButton = {
            TextButton(
                onClick = { selected?.let(viewModel::sendShare) },
                enabled = selected != null && !state.shareSending && (share.text.isNotBlank() || share.files.isNotEmpty()),
            ) { Text(if (state.shareSending) ui("发送中") else ui("发送")) }
        },
        dismissButton = {
            TextButton(onClick = viewModel::dismissShare, enabled = !state.shareSending) { Text(ui("取消")) }
        },
    )
}
