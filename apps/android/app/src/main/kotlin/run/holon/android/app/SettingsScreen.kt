@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue

import android.app.Activity
import android.content.Context
import android.content.Intent
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider

@Composable
internal fun SettingsScreen(state: HolonUiState, viewModel: SettingsActions, onBack: () -> Unit) {
    var showDiagnostics by remember { mutableStateOf(false) }
    var showLanguagePicker by remember { mutableStateOf(false) }
    var pendingSignOut by remember { mutableStateOf<String?>(null) }
    var pendingNetworkDeletion by remember { mutableStateOf<NetworkProfile?>(null) }
    if (showLanguagePicker) AppLanguagePicker { showLanguagePicker = false }
    pendingNetworkDeletion?.let { profile ->
        DeleteNetworkDialog(
            profile = profile,
            isCurrent = profile.networkId == state.session?.networkId,
            busy = state.busy || state.enqueueing || state.stagingAttachment,
            onConfirm = {
                pendingNetworkDeletion = null
                viewModel.deleteNetwork(profile.networkId)
            },
            onDismiss = { pendingNetworkDeletion = null },
        )
    }
    pendingSignOut?.let { action ->
        AlertDialog(
            onDismissRequest = { pendingSignOut = null },
            title = { Text(if (action == "relogin") ui("重新登录当前主机？") else ui("退出并清除本机数据？")) },
            text = { Text(ui("本机缓存、草稿和待发送附件会被清除。已发送给主机的工作不会撤回。")) },
            confirmButton = {
                TextButton(onClick = {
                    pendingSignOut = null
                    if (action == "relogin") viewModel.relogin() else viewModel.logout()
                }) { Text(if (action == "relogin") ui("重新登录") else ui("退出登录")) }
            },
            dismissButton = { TextButton(onClick = { pendingSignOut = null }) { Text(ui("取消")) } },
        )
    }
    Scaffold(
        modifier = Modifier.fillMaxSize(),
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text(ui("设置")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = ui("返回"))
                    }
                },
                actions = {
                    IconButton(onClick = viewModel::refresh, enabled = !state.busy) {
                        Icon(Icons.Default.Refresh, contentDescription = ui("刷新"))
                    }
                },
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier.padding(padding),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 16.dp, vertical = 4.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item {
                HolonSection(ui("当前连接")) {
                    SettingsValue(ui("地址"), state.session?.baseUrl.orEmpty())
                    SettingsValue(ui("状态"), if (state.online) ui("已连接") else ui("离线缓存"))
                    state.lastSyncedAt?.let { SettingsValue(ui("上次同步"), syncClock(it)) }
                }
            }
            item {
                NetworkSection(
                    profiles = state.networkProfiles,
                    currentNetworkId = state.session?.networkId,
                    busy = state.busy || state.enqueueing || state.stagingAttachment,
                    switchingNetworkId = state.switchingNetworkId,
                    onSwitch = viewModel::switchNetwork,
                    onAdd = viewModel::beginAddNetwork,
                    onDelete = { networkId -> pendingNetworkDeletion = state.networkProfiles.firstOrNull { it.networkId == networkId } },
                    statusMessage = state.statusMessage,
                )
            }
            item {
                HolonSection(ui("当前身份")) {
                    SettingsValue(ui("用户"), state.session?.user?.displayName ?: state.session?.user?.userId.orEmpty())
                    Text(ui("原始访问令牌不落盘；可撤销的会话凭据在设备上加密保存。"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            item {
                HolonSection(ui("关于")) {
                    val uriHandler = androidx.compose.ui.platform.LocalUriHandler.current
                    SettingsValue(ui("App"), BuildConfig.VERSION_NAME)
                    Text(ui("连接已有 Holon 主机的移动工作台。打开应用后同步最新状态。"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    TextButton(onClick = {
                        uriHandler.openUri(
                            if (UiCopy.effectiveLanguage().startsWith("zh", ignoreCase = true)) "https://holon.run/zh-CN/privacy"
                            else "https://holon.run/privacy",
                        )
                    }) {
                        Text(ui("隐私政策"))
                    }
                }
            }
            item {
                HolonSection(ui("语言")) {
                    TextButton(onClick = { showLanguagePicker = true }) {
                        Text(ui("应用语言") + " · " + when (UiCopy.preference()) {
                            "en" -> "English"
                            "zh" -> "简体中文"
                            else -> ui("系统默认")
                        })
                    }
                }
            }
            item {
                TextButton(onClick = { showDiagnostics = !showDiagnostics }, modifier = Modifier.fillMaxWidth()) {
                    Text(if (showDiagnostics) ui("收起连接诊断") else ui("查看连接诊断"))
                }
            }
            if (showDiagnostics) item {
                HolonSection(ui("连接诊断")) {
                    SettingsValue(ui("Runtime"), state.session?.runtimeId.orEmpty())
                    SettingsValue(ui("认证"), state.session?.user?.authMethod.orEmpty())
                    SettingsValue(ui("协议"), "holon-control/1")
                    SettingsValue(ui("能力"), state.session?.server?.capabilities?.size?.toString().orEmpty())
                    Text(
                        state.session?.server?.capabilities?.sorted()?.joinToString("\n").orEmpty(),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    val context = LocalContext.current
                    TextButton(onClick = { shareDiagnostics(context, state) }) {
                        Text(ui("分享脱敏诊断信息"))
                    }
                    val traceSummary = viewModel.traceRecorder.summary()
                    SettingsValue(
                        ui("Trace"),
                        "${traceSummary.eventCount} ${ui("条")} · ${traceSummary.bytes} B",
                    )
                    TextButton(onClick = { shareTrace(context, viewModel.traceRecorder) }) {
                        Text(ui("导出并分享 Trace"))
                    }
                    TextButton(onClick = viewModel::shareTraceWithAgent) {
                        Text(ui("发送 Trace 给 Agent"))
                    }
                }
            }
            item {
                OutlinedButton(
                    onClick = { pendingSignOut = "relogin" },
                    enabled = !state.busy,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text(ui("重新登录当前主机")) }
            }
            item {
                OutlinedButton(
                    onClick = { pendingSignOut = "logout" },
                    enabled = !state.busy,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text(ui("退出并清除本机数据")) }
            }
            item { Spacer(Modifier.height(16.dp)) }
        }
    }
}

@Composable
internal fun NetworkSection(
    profiles: List<NetworkProfile>,
    currentNetworkId: String?,
    busy: Boolean,
    switchingNetworkId: String?,
    onSwitch: (String) -> Unit,
    onAdd: () -> Unit,
    onDelete: (String) -> Unit,
    statusMessage: String? = null,
) {
    HolonSection(ui("网络")) {
        statusMessage?.let {
            Text(ui(it), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        profiles.sortedByDescending { it.networkId == currentNetworkId }.forEach { profile ->
            val isCurrent = profile.networkId == currentNetworkId
            SavedNetworkRow(
                profile = profile,
                isCurrent = isCurrent,
                busy = busy,
                onSwitch = { onSwitch(profile.networkId) },
                onDelete = { onDelete(profile.networkId) },
            )
        }
        OutlinedButton(onClick = onAdd, enabled = !busy, modifier = Modifier.fillMaxWidth()) {
            Icon(Icons.Default.Add, contentDescription = null)
            Spacer(Modifier.width(8.dp))
            Text(ui("添加网络"))
        }
        switchingNetworkId?.let {
            Text(
                ui("正在切换网络…"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
internal fun AppLanguagePicker(onDismiss: () -> Unit) {
    val context = LocalContext.current
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(ui("应用语言")) },
        text = {
            Column {
                listOf(null to ui("系统默认"), "en" to "English", "zh" to "简体中文").forEach { (language, label) ->
                    TextButton(onClick = {
                        UiCopy.select(context, language)
                        onDismiss()
                        (context as? Activity)?.recreate()
                    }) { Text(label) }
                }
            }
        },
        confirmButton = {},
    )
}

internal fun shareDiagnostics(context: Context, state: HolonUiState) {
    val server = state.session?.server
    val details = buildString {
        appendLine("Holon Android ${BuildConfig.VERSION_NAME}")
        appendLine(ui("状态：${if (state.online) "已连接" else "离线缓存"}"))
        appendLine(ui("协议：holon-control/1"))
        appendLine(ui("认证方式：${server?.authMode ?: "未知"}"))
        appendLine(ui("能力：${server?.capabilities?.sorted()?.joinToString(", ").orEmpty()}"))
        server?.limits?.let {
            appendLine(ui("附件限制：图片 ${it.promptImageAttachmentMaxBytes} B，文件 ${it.promptFileAttachmentMaxBytes} B"))
        }
        append(ui("不包含地址、用户、session、token 或会话正文。"))
    }
    val intent = Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_TEXT, details)
    }
    context.startActivity(Intent.createChooser(intent, ui("分享连接诊断")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
}

internal fun shareTrace(context: Context, recorder: TraceRecorder) {
    val file = recorder.export()
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
    val intent =
        Intent(Intent.ACTION_SEND).apply {
            type = "application/x-ndjson"
            putExtra(Intent.EXTRA_STREAM, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
    context.startActivity(Intent.createChooser(intent, ui("分享 Trace")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
}

@Composable
internal fun SettingsValue(label: String, value: String) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Text(label, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.width(72.dp))
        Text(value.ifBlank { "—" }, modifier = Modifier.weight(1f))
    }
}
