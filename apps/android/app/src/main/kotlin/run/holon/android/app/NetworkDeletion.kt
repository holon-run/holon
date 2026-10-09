package run.holon.android.app

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.DeleteOutline
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

@Composable
internal fun SavedNetworkRow(
    profile: NetworkProfile,
    isCurrent: Boolean,
    busy: Boolean,
    onSwitch: () -> Unit,
    onDelete: () -> Unit,
) {
    Surface(
        modifier = Modifier.fillMaxWidth().clickable(enabled = !busy && !isCurrent, onClick = onSwitch),
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, if (isCurrent) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outlineVariant),
        color = MaterialTheme.colorScheme.surface,
    ) {
        Row(Modifier.fillMaxWidth().padding(start = 12.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text(profile.displayName, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(
                    profile.baseUrl,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (isCurrent) {
                Icon(Icons.Default.CheckCircle, contentDescription = ui("当前网络"), tint = MaterialTheme.colorScheme.primary)
            }
            IconButton(onClick = onDelete, enabled = !busy) {
                Icon(
                    Icons.Default.DeleteOutline,
                    contentDescription = "${ui("删除网络")} ${profile.displayName}",
                    tint = MaterialTheme.colorScheme.error,
                )
            }
        }
    }
}

@Composable
internal fun DeleteNetworkDialog(
    profile: NetworkProfile,
    isCurrent: Boolean,
    busy: Boolean,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(ui("删除此网络？")) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(profile.displayName, style = MaterialTheme.typography.titleSmall)
                Text(profile.baseUrl, style = MaterialTheme.typography.bodySmall)
                Text(ui("此网络的本机配置、登录凭据、缓存、草稿、待发送消息和附件及诊断记录会被清除。远端主机和已发送的工作不受影响。"))
                if (isCurrent) Text(ui("这是当前网络。删除后将断开连接并返回登录页，不会自动连接其他网络。"))
            }
        },
        confirmButton = {
            TextButton(onClick = onConfirm, enabled = !busy) {
                Text(ui("删除网络"), color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(ui("取消")) } },
    )
}
