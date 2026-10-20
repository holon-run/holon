package run.holon.android.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.Json

@Composable
internal fun SourceInspectionScreen(source: SourceInspection, onBack: () -> Unit, onRetry: () -> Unit) {
    var showRaw by rememberSaveable(source.input.messageId) { mutableStateOf(false) }
    LazyColumn(
        modifier = Modifier.fillMaxSize().testTag("source-inspection"),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            TextButton(onClick = onBack) { Text(ui("返回会话")) }
            Text(ui("来源详情"), style = MaterialTheme.typography.headlineSmall)
            SelectionContainer { Text(source.input.messageId, style = MaterialTheme.typography.labelSmall) }
        }
        if (source.loading) item { CircularProgressIndicator() }
        source.failure?.let { failure ->
            item {
                Text(ui(when (failure) {
                    SourceFailure.Unauthorized -> "登录已失效，请重新登录后查看来源"
                    SourceFailure.Forbidden -> "没有权限读取这条来源消息"
                    SourceFailure.Missing -> "来源消息不存在或已不可读取"
                    SourceFailure.Offline -> "当前离线，无法读取完整来源"
                    SourceFailure.Unavailable -> "来源暂不可用，请重试"
                }))
                TextButton(onClick = onRetry) { Text(ui("重试")) }
                if (source.input.preview.isNotBlank()) {
                    Text(ui("预览（非完整来源）"), style = MaterialTheme.typography.titleSmall)
                    SelectionContainer { Text(source.input.preview) }
                }
            }
        }
        source.message?.let { message ->
            item {
                Text(ui("消息正文"), style = MaterialTheme.typography.titleMedium)
                val bodyText = message.body.text ?: message.body.value?.let(::sourceJsonText)
                    ?: sourceJsonText(message.body.raw)
                SelectionContainer { Text(bodyText.ifBlank { ui("没有文本正文") }) }
            }
            item {
                Text(ui("来源与身份"), style = MaterialTheme.typography.titleMedium)
                // These are the message's canonical values, never the turn's icon classification.
                SelectionContainer {
                    Text(listOfNotNull(
                        "origin: ${message.origin.kind}", "authority_class: ${message.authorityClass}",
                        "agent_id: ${message.agentId}", "kind: ${message.kind}", "priority: ${message.priority}",
                        "created_at: ${message.createdAt}",
                        message.origin.actorId?.let { "actor_id: $it" },
                        message.origin.actorDisplayName?.let { "actor: $it" },
                        message.origin.channelId?.let { "channel_id: $it" },
                        message.origin.senderId?.let { "sender_id: $it" },
                        message.triggerKind?.let { "trigger_kind: $it" },
                    ).joinToString("\n"))
                }
                TextButton(onClick = { showRaw = !showRaw }) {
                    Text(ui(if (showRaw) "隐藏结构化信息" else "查看结构化信息"))
                }
            }
            if (showRaw) item {
                SelectionContainer { Text(sourceJsonText(message.raw), style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}

private val sourceJson = Json { prettyPrint = true }

private fun sourceJsonText(value: kotlinx.serialization.json.JsonElement): String =
    sourceJson.encodeToString(kotlinx.serialization.json.JsonElement.serializer(), value)
