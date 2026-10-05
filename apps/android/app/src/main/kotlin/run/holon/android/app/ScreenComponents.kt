@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.InsertDriveFile
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.Image
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import java.io.File
import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.UUID
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonWorkItemSnapshot
import run.holon.android.sdk.HolonWorkspaceEntry

@Composable
internal fun ErrorBanner(message: String, onDismiss: (() -> Unit)?) {
    Surface(
        color = MaterialTheme.colorScheme.errorContainer,
        contentColor = MaterialTheme.colorScheme.onErrorContainer,
        shape = RoundedCornerShape(9.dp),
        modifier = Modifier.fillMaxWidth().padding(horizontal = if (onDismiss == null) 0.dp else 8.dp),
    ) {
        Row(Modifier.padding(horizontal = 12.dp, vertical = 9.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                ui(message),
                modifier = Modifier.weight(1f),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onErrorContainer,
            )
            onDismiss?.let {
                TextButton(
                    onClick = it,
                    colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.onErrorContainer),
                ) {
                    Text(ui("关闭"))
                }
            }
        }
    }
}

@Composable
internal fun EmptyPage(title: String, text: String) {
    Box(Modifier.fillMaxWidth().padding(28.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            Text(text, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
internal fun CompactStatus(label: String, tone: StatusTone) {
    val color = toneColor(tone)
    Row(
        modifier = Modifier.padding(horizontal = 6.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(7.dp).background(color, RoundedCornerShape(99.dp)))
        Text(label, style = MaterialTheme.typography.labelSmall, color = color)
    }
}

internal fun AgentSummary.isActive(): Boolean =
    schedulingPosture in setOf("active_turn", "has_queued_input", "has_runnable_work") ||
        runtimeStatus.lowercase() in setOf("running", "active")

internal fun plainTextPreview(markdown: String): String =
    markdown
        .replace(Regex("\\[([^]]+)]\\([^)]*\\)"), "${'$'}1")
        .replace(Regex("(?m)^\\s{0,3}#{1,6}\\s+"), "")
        .replace(Regex("[`*_~>]"), "")
        .replace(Regex("\\s+"), " ")
        .trim()

internal fun relativeTime(value: String): String {
    val duration = runCatching { Duration.between(Instant.parse(value), Instant.now()) }.getOrNull() ?: return value
    val seconds = duration.seconds.coerceAtLeast(0)
    return when {
        seconds < 60 -> ui("刚刚")
        seconds < 3_600 -> ui("${seconds / 60} 分钟前")
        seconds < 86_400 -> ui("${seconds / 3_600} 小时前")
        seconds < 604_800 -> ui("${seconds / 86_400} 天前")
        else -> value.take(10)
    }
}

internal fun syncClock(epochMillis: Long): String =
    Instant.ofEpochMilli(epochMillis).atZone(ZoneId.systemDefault()).format(DateTimeFormatter.ofPattern("HH:mm"))

internal fun relativeFileTime(value: Long): String {
    val epochMillis = if (value > 10_000_000_000L) value else value * 1_000L
    val duration = Duration.between(Instant.ofEpochMilli(epochMillis), Instant.now())
    val seconds = duration.seconds.coerceAtLeast(0)
    return when {
        seconds < 60 -> ui("刚刚")
        seconds < 3_600 -> ui("${seconds / 60} 分钟前")
        seconds < 86_400 -> ui("${seconds / 3_600} 小时前")
        seconds < 604_800 -> ui("${seconds / 86_400} 天前")
        else -> ui("${seconds / 604_800} 周前")
    }
}

internal fun workItemStatusLabel(item: HolonWorkItemSnapshot): String =
    when {
        item.blockedBy != null -> ui("受阻")
        item.state == "completed" -> ui("已完成")
        item.readiness == "ready" -> ui("可继续")
        item.schedulingState == "waiting" -> ui("等待中")
        else -> item.readiness ?: item.state
    }

internal fun HolonWorkItemSnapshot.listSummary(): String =
    if (state == "completed") {
        resultSummary?.takeIf(String::isNotBlank)?.let(::plainTextPreview) ?: ui("查看工作详情")
    } else {
        focus
            ?.takeUnless {
                it.equals(state, ignoreCase = true) ||
                    it.equals(readiness, ignoreCase = true) ||
                    it.equals(schedulingState, ignoreCase = true)
            }
            ?: resultSummary?.takeIf(String::isNotBlank)?.let(::plainTextPreview)
            ?: ui("等待更多信息")
    }

internal fun workItemTone(item: HolonWorkItemSnapshot): StatusTone =
    when {
        item.blockedBy != null -> StatusTone.Danger
        item.state == "completed" -> StatusTone.Success
        item.schedulingState == "waiting" -> StatusTone.Neutral
        else -> StatusTone.Accent
    }

internal fun fileIcon(entry: HolonWorkspaceEntry): ImageVector =
    when {
        entry.type == "directory" -> Icons.Default.Folder
        entry.mediaType?.startsWith("image/") == true -> Icons.Default.Image
        entry.mediaType?.startsWith("text/") == true || entry.name.endsWith(".md", true) -> Icons.Default.Description
        else -> Icons.AutoMirrored.Filled.InsertDriveFile
    }

internal fun AgentSummary.statusTone(): StatusTone =
    when {
        needsReply() -> StatusTone.Warning
        schedulingPosture == "blocked" -> StatusTone.Danger
        schedulingPosture == "active_turn" || runtimeStatus.lowercase() in setOf("running", "active") -> StatusTone.Accent
        schedulingPosture in setOf("has_queued_input", "has_runnable_work") -> StatusTone.Accent
        else -> StatusTone.Neutral
    }

internal fun AgentSummary.statusLabel(): String =
    when {
        needsReply() -> ui("等你回应")
        schedulingPosture == "active_turn" -> ui("工作中")
        schedulingPosture == "has_queued_input" -> ui("已排队")
        schedulingPosture == "has_runnable_work" -> ui("待运行")
        schedulingPosture == "waiting_for_external" -> ui("等外部变化")
        schedulingPosture == "waiting_for_task" -> ui("等任务结果")
        schedulingPosture == "blocked" -> ui("受阻")
        schedulingPosture == "idle" -> ui("空闲")
        runtimeStatus.lowercase() == "offline" -> ui("离线缓存")
        else -> runtimeStatus
    }

@Composable
internal fun toneColor(tone: StatusTone): Color =
    when (tone) {
        StatusTone.Neutral -> MaterialTheme.colorScheme.onSurfaceVariant
        StatusTone.Accent -> MaterialTheme.colorScheme.primary
        StatusTone.Success -> HolonSuccess
        StatusTone.Warning -> HolonWarning
        StatusTone.Danger -> MaterialTheme.colorScheme.error
    }

internal fun createCameraUri(context: Context): Uri {
    val directory = File(context.cacheDir, "camera").apply { mkdirs() }
    val file = File(directory, "${UUID.randomUUID()}.jpg")
    return FileProvider.getUriForFile(context, "${context.packageName}.files", file)
}

internal fun shareArtifact(context: Context, artifact: PreparedArtifact) {
    val uri = FileProvider.getUriForFile(
        context,
        "${context.packageName}.files",
        File(artifact.localPath),
    )
    val intent =
        Intent(Intent.ACTION_SEND).apply {
            type = artifact.mediaType
            putExtra(Intent.EXTRA_STREAM, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
    context.startActivity(Intent.createChooser(intent, ui("分享 ${artifact.fileName}")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
}

internal fun formatBytes(value: Long): String =
    when {
        value >= 1024 * 1024 -> "%.1f MB".format(value / 1024.0 / 1024.0)
        value >= 1024 -> "%.1f KB".format(value / 1024.0)
        else -> "$value B"
    }
