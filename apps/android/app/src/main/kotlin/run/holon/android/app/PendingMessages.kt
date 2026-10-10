package run.holon.android.app

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Inbox
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import run.holon.android.sdk.HolonPendingInput

@Composable
internal fun PendingMessagesCard(
    inputs: List<HolonPendingInput>,
    expanded: Boolean,
    onToggle: () -> Unit,
    onInput: (HolonPendingInput) -> Unit,
) {
    if (inputs.isEmpty()) return
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column {
            Row(
                modifier = Modifier.fillMaxWidth()
                    .semantics { stateDescription = ui(if (expanded) "已展开" else "已折叠") }
                    .clickable(role = Role.Button, onClickLabel = ui(if (expanded) "收起消息" else "展开消息"), onClick = onToggle)
                    .padding(12.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Icon(Icons.Default.Inbox, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("${ui("后台消息")} · ${inputs.size}", style = MaterialTheme.typography.labelLarge)
                    Text(
                        if (expanded) ui("点击消息查看详情") else inputs.first().let { it.taskResult?.summary ?: it.preview }.ifBlank { ui("暂无消息预览") },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Icon(if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore, contentDescription = null)
            }
            if (expanded) {
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                Column(Modifier.heightIn(max = 320.dp).verticalScroll(rememberScrollState())) {
                    inputs.forEachIndexed { index, input ->
                        if (index > 0) HorizontalDivider(Modifier.padding(horizontal = 12.dp), color = MaterialTheme.colorScheme.outlineVariant)
                        Column(
                            Modifier.fillMaxWidth().clickable(role = Role.Button, onClickLabel = ui("消息详情")) { onInput(input) }
                                .padding(12.dp),
                            verticalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            PendingMessageMeta(input)
                            Text(
                                (input.taskResult?.summary ?: input.preview).ifBlank { ui("暂无消息预览") },
                                style = MaterialTheme.typography.bodySmall,
                                maxLines = 2,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
internal fun PendingMessageDetails(input: HolonPendingInput, onOpenTask: ((run.holon.android.sdk.HolonTaskSnapshot) -> Unit)? = null) {
    Column(
        Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(ui("消息详情"), style = MaterialTheme.typography.titleLarge)
        PendingMessageMeta(input)
        if (input.taskResult != null && onOpenTask != null) {
            TaskResultProcessRow(run.holon.android.sdk.HolonTurnInput(input.messageId, input.preview, input.actorDisplayName, input.presentationClass, input.createdAt, taskResult = input.taskResult), input.createdAt, onOpenTask)
        } else SelectionContainer {
            Text(input.preview.ifBlank { ui("暂无消息预览") }, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
private fun PendingMessageMeta(input: HolonPendingInput) {
    Text(
        listOfNotNull(
            ui(if (input.state == "queued") "排队中" else "待处理"),
            input.taskResult?.let(::taskResultStatus),
            input.actorDisplayName?.takeIf { it.isNotBlank() },
            input.createdAt?.let(::localTimestamp),
        ).joinToString(" · "),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}
