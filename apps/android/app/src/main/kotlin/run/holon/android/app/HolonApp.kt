@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.InsertDriveFile
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.AttachFile
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Image
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.material.icons.filled.RadioButtonUnchecked
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.filled.Settings
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.Image
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.produceState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import java.io.File
import java.io.IOException
import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonWorkItemSnapshot
import run.holon.android.sdk.HolonWorkspaceEntry

@Composable
internal fun HolonApp(viewModel: HolonViewModel) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    Surface(
        modifier = Modifier.fillMaxSize(),
        color = MaterialTheme.colorScheme.background,
        contentColor = MaterialTheme.colorScheme.onBackground,
    ) {
        when (state.phase) {
            AppPhase.Starting -> StartingScreen()
            AppPhase.SignedOut -> LoginScreen(state, viewModel)
            AppPhase.Ready -> MainShell(state, viewModel)
        }
    }
}

@Composable
private fun StartingScreen() {
    Box(
        Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(16.dp)) {
            HolonMark()
            CircularProgressIndicator(modifier = Modifier.size(24.dp), strokeWidth = 2.dp)
            Text("正在恢复 Holon…", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun LoginScreen(state: HolonUiState, viewModel: HolonViewModel) {
    val isInsecureHttp = state.baseUrl.trim().startsWith("http://", ignoreCase = true)
    Column(
        Modifier.fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .statusBarsPadding()
            .navigationBarsPadding()
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp),
        verticalArrangement = Arrangement.Center,
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(vertical = 24.dp),
            verticalArrangement = Arrangement.spacedBy(18.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                HolonMark()
                Column {
                    Text("连接 Holon", style = MaterialTheme.typography.headlineLarge)
                    Text("继续你正在进行的工作", color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            Spacer(Modifier.height(4.dp))
            Text("需要一台已运行的 Holon 主机，以及该主机提供的访问令牌。", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            OutlinedTextField(
                value = state.baseUrl,
                onValueChange = viewModel::setBaseUrl,
                label = { Text("Holon 地址") },
                supportingText = {
                    if (isInsecureHttp) {
                        Text(
                            "HTTP 本身不加密；请只在可信局域网或 Tailscale 等加密隧道中使用",
                            color = MaterialTheme.colorScheme.tertiary,
                        )
                    } else {
                        Text("例如 https://holon.example.com 或 http://100.64.0.1:7878")
                    }
                },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
                singleLine = true,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            )
            AnimatedVisibility(visible = isInsecureHttp) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Checkbox(
                        checked = state.allowInsecureHttp,
                        onCheckedChange = viewModel::setAllowInsecureHttp,
                        enabled = !state.busy,
                    )
                    Text(
                        "我确认此地址位于可信网络或加密隧道中",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
            OutlinedTextField(
                value = state.token,
                onValueChange = viewModel::setToken,
                label = { Text("访问令牌（token）") },
                supportingText = { Text("登录后只保存可撤销的会话，不保存原始令牌") },
                trailingIcon = {
                    TextButton(onClick = viewModel::toggleToken) {
                        Text(if (state.showToken) "隐藏" else "显示")
                    }
                },
                visualTransformation = if (state.showToken) VisualTransformation.None else PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { viewModel.login() }),
                singleLine = true,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            )
            state.error?.let { ErrorBanner(it, viewModel::clearError) }
            Button(
                onClick = viewModel::login,
                enabled =
                    !state.busy &&
                        state.token.isNotBlank() &&
                        (!isInsecureHttp || state.allowInsecureHttp),
                modifier = Modifier.fillMaxWidth().height(52.dp),
                shape = RoundedCornerShape(10.dp),
            ) {
                if (state.busy) {
                    CircularProgressIndicator(Modifier.size(18.dp), color = MaterialTheme.colorScheme.onPrimary, strokeWidth = 2.dp)
                    Spacer(Modifier.width(10.dp))
                }
                Text(if (state.busy) "正在登录" else "登录")
            }
            Text(
                "HTTPS 默认安全；HTTP 需要逐次确认。浏览器登录和扫码配对将在后续版本提供。",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun MainShell(state: HolonUiState, viewModel: HolonViewModel) {
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val useListDetail = maxWidth >= 840.dp && state.mainDestination == MainDestination.Agents
        when {
            useListDetail -> {
                Row(Modifier.fillMaxSize()) {
                    Box(Modifier.width(380.dp).fillMaxHeight()) {
                        AgentsScreen(state, viewModel)
                    }
                    Box(Modifier.width(1.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant))
                    Box(Modifier.weight(1f).fillMaxHeight()) {
                        if (state.selectedAgent != null) {
                            ConversationScreen(state, viewModel)
                        } else {
                            EmptyPage("选择一个 Agent", "结果、工作和文件将在这里打开。")
                        }
                    }
                }
            }
            state.selectedAgent != null -> ConversationScreen(state, viewModel)
            state.mainDestination == MainDestination.Settings -> SettingsScreen(
                state = state,
                viewModel = viewModel,
                onBack = { viewModel.selectMainDestination(MainDestination.Agents) },
            )
            else -> AgentsScreen(state, viewModel)
        }
    }
}

private enum class AgentFilter(val label: String) {
    All("全部"),
    Attention("需回应"),
    NewResults("新结果"),
    Active("工作中"),
}

@Composable
private fun AgentsScreen(state: HolonUiState, viewModel: HolonViewModel) {
    var filter by remember { mutableStateOf(AgentFilter.All) }
    var searchOpen by remember { mutableStateOf(state.search.isNotBlank()) }
    val attentionCount = state.agents.count { it.needsReply() }
    val unreadCount = if (state.readBriefsLoaded) state.agents.count { it.hasUnreadBrief(state.readBriefIds) } else 0
    val activeCount = state.agents.count { it.isActive() }
    val filtered = state.filteredAgents.filter { agent ->
        when (filter) {
            AgentFilter.All -> true
            AgentFilter.Attention -> agent.needsReply()
            AgentFilter.NewResults -> state.readBriefsLoaded && agent.hasUnreadBrief(state.readBriefIds)
            AgentFilter.Active -> agent.isActive()
        }
    }
    Scaffold(
        modifier = Modifier.fillMaxSize(),
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("Holon", style = MaterialTheme.typography.titleMedium)
                        Text(
                            "${state.agents.size} 个 Agent · ${if (state.online) "已同步" else "离线缓存"}" +
                                (state.lastSyncedAt?.let { " ${syncClock(it)}" } ?: ""),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                actions = {
                    IconButton(onClick = {
                        if (searchOpen) viewModel.setSearch("")
                        searchOpen = !searchOpen
                    }) {
                        Icon(if (searchOpen) Icons.Default.Close else Icons.Default.Search, contentDescription = if (searchOpen) "关闭搜索" else "搜索")
                    }
                    IconButton(onClick = { viewModel.selectMainDestination(MainDestination.Settings) }) {
                        Icon(Icons.Default.Settings, contentDescription = "设置")
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            state.statusMessage?.let { Text(it, modifier = Modifier.padding(horizontal = 16.dp), style = MaterialTheme.typography.bodySmall) }
            state.error?.let { ErrorBanner(it, null) }
            AnimatedVisibility(searchOpen) {
                OutlinedTextField(
                    value = state.search,
                    onValueChange = viewModel::setSearch,
                    placeholder = { Text("搜索名称或 ID") },
                    leadingIcon = { Icon(Icons.Default.Search, contentDescription = null) },
                    trailingIcon = {
                        if (state.search.isNotBlank()) IconButton(onClick = { viewModel.setSearch("") }) {
                            Icon(Icons.Default.Close, contentDescription = "清除")
                        }
                    },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
                )
            }
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                AgentFilter.entries.forEach { option ->
                    val count = when (option) {
                        AgentFilter.All -> state.agents.size
                        AgentFilter.Attention -> attentionCount
                        AgentFilter.NewResults -> unreadCount
                        AgentFilter.Active -> activeCount
                    }
                    FilterChip(
                        selected = filter == option,
                        onClick = { filter = option },
                        label = { Text("${option.label} $count") },
                    )
                }
            }
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(filtered, key = AgentSummary::id) { agent ->
                    AgentConversationRow(agent, unread = state.readBriefsLoaded && agent.hasUnreadBrief(state.readBriefIds), onClick = { viewModel.openAgent(agent) })
                    HorizontalDivider(modifier = Modifier.padding(start = 16.dp), color = MaterialTheme.colorScheme.outlineVariant)
                }
                if (filtered.isEmpty()) item {
                    when {
                        state.agents.isEmpty() -> EmptyPage("还没有 Agent", "连接成功后，Agent 会显示在这里。")
                        filter == AgentFilter.Attention && attentionCount == 0 ->
                            EmptyPage("目前没有需要回应的 Agent", "选择“全部”查看其他 Agent。")
                        filter == AgentFilter.NewResults && unreadCount == 0 ->
                            EmptyPage("目前没有未读结果", "选择“全部”查看其他 Agent。")
                        filter == AgentFilter.Active && activeCount == 0 ->
                            EmptyPage("目前没有工作中的 Agent", "选择“全部”查看其他 Agent。")
                        state.search.isNotBlank() ->
                            EmptyPage("没有匹配的 Agent", "调整搜索词或筛选条件后再试。")
                        else -> EmptyPage("这个筛选暂无 Agent", "选择“全部”查看所有 Agent。")
                    }
                }
                item { Spacer(Modifier.height(20.dp)) }
            }
        }
    }
}

@Composable
internal fun AgentConversationRow(agent: AgentSummary, compact: Boolean = false, unread: Boolean = false, onClick: () -> Unit) {
    val tone = agent.statusTone()
    val briefPreview = plainTextPreview(agent.latestBrief?.preview.orEmpty())
    val postureReason = agent.postureReason.orEmpty()
    Surface(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
        color = MaterialTheme.colorScheme.background,
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = if (compact) 10.dp else 12.dp),
            verticalArrangement = Arrangement.spacedBy(5.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(agent.displayName, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                CompactStatus(agent.statusLabel(), tone)
            }
            Text(
                when {
                    agent.needsReply() -> "正在等你回应"
                    briefPreview.isNotBlank() -> briefPreview
                    postureReason.isNotBlank() -> plainTextPreview(postureReason)
                    else -> "暂无新的工作摘要"
                },
                maxLines = if (compact) 1 else 2,
                overflow = TextOverflow.Ellipsis,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodyMedium,
            )
            if (!compact) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    if (unread) Text("新结果", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
                    Text(
                        listOfNotNull(
                            agent.latestBrief?.createdAt?.let(::relativeTime),
                            agent.currentWorkItemId?.let { "有进行中的 WorkItem" },
                        ).joinToString(" · ").ifBlank { "尚无活动" },
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun SettingsScreen(state: HolonUiState, viewModel: HolonViewModel, onBack: () -> Unit) {
    var showDiagnostics by remember { mutableStateOf(false) }
    var pendingSignOut by remember { mutableStateOf<String?>(null) }
    pendingSignOut?.let { action ->
        AlertDialog(
            onDismissRequest = { pendingSignOut = null },
            title = { Text(if (action == "relogin") "重新登录当前主机？" else "退出并清除本机数据？") },
            text = { Text("本机缓存、草稿和待发送附件会被清除。已发送给主机的工作不会撤回。") },
            confirmButton = {
                TextButton(onClick = {
                    pendingSignOut = null
                    if (action == "relogin") viewModel.relogin() else viewModel.logout()
                }) { Text(if (action == "relogin") "重新登录" else "退出登录") }
            },
            dismissButton = { TextButton(onClick = { pendingSignOut = null }) { Text("取消") } },
        )
    }
    Scaffold(
        modifier = Modifier.fillMaxSize(),
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = { Text("设置") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回")
                    }
                },
                actions = {
                    IconButton(onClick = viewModel::refresh, enabled = !state.busy) {
                        Icon(Icons.Default.Refresh, contentDescription = "刷新")
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
                HolonSection("当前连接") {
                    SettingsValue("地址", state.session?.baseUrl.orEmpty())
                    SettingsValue("状态", if (state.online) "已连接" else "离线缓存")
                    state.lastSyncedAt?.let { SettingsValue("上次同步", syncClock(it)) }
                }
            }
            item {
                HolonSection("当前身份") {
                    SettingsValue("用户", state.session?.user?.displayName ?: state.session?.user?.userId.orEmpty())
                    Text("访问令牌不保存在设备上。", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            item {
                HolonSection("关于") {
                    SettingsValue("App", BuildConfig.VERSION_NAME)
                    Text("连接已有 Holon 主机的移动工作台。打开应用后同步最新状态。", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            item {
                TextButton(onClick = { showDiagnostics = !showDiagnostics }, modifier = Modifier.fillMaxWidth()) {
                    Text(if (showDiagnostics) "收起连接诊断" else "查看连接诊断")
                }
            }
            if (showDiagnostics) item {
                HolonSection("连接诊断") {
                    SettingsValue("Runtime", state.session?.runtimeId.orEmpty())
                    SettingsValue("认证", state.session?.user?.authMethod.orEmpty())
                    SettingsValue("协议", "holon-control/1")
                    SettingsValue("能力", state.session?.server?.capabilities?.size?.toString().orEmpty())
                    Text(
                        state.session?.server?.capabilities?.sorted()?.joinToString("\n").orEmpty(),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    val context = LocalContext.current
                    TextButton(onClick = { shareDiagnostics(context, state) }) {
                        Text("分享脱敏诊断信息")
                    }
                }
            }
            item {
                OutlinedButton(
                    onClick = { pendingSignOut = "relogin" },
                    enabled = !state.busy,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("重新登录当前主机") }
            }
            item {
                OutlinedButton(
                    onClick = { pendingSignOut = "logout" },
                    enabled = !state.busy,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("退出并清除本机数据") }
            }
            item { Spacer(Modifier.height(16.dp)) }
        }
    }
}

private fun shareDiagnostics(context: Context, state: HolonUiState) {
    val server = state.session?.server
    val details = buildString {
        appendLine("Holon Android ${BuildConfig.VERSION_NAME}")
        appendLine("状态：${if (state.online) "已连接" else "离线缓存"}")
        appendLine("协议：holon-control/1")
        appendLine("认证方式：${server?.authMode ?: "未知"}")
        appendLine("能力：${server?.capabilities?.sorted()?.joinToString(", ").orEmpty()}")
        server?.limits?.let {
            appendLine("附件限制：图片 ${it.promptImageAttachmentMaxBytes} B，文件 ${it.promptFileAttachmentMaxBytes} B")
        }
        append("不包含地址、用户、session、token 或会话正文。")
    }
    val intent = Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_TEXT, details)
    }
    context.startActivity(Intent.createChooser(intent, "分享连接诊断").addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
}

@Composable
private fun SettingsValue(label: String, value: String) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Text(label, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.width(72.dp))
        Text(value.ifBlank { "—" }, modifier = Modifier.weight(1f))
    }
}

@Composable
private fun ConversationScreen(state: HolonUiState, viewModel: HolonViewModel) {
    val agent = state.selectedAgent ?: return
    val timelinePosition = remember(agent.id) { ConversationTimelinePosition() }
    val workListState = remember(agent.id) { LazyListState() }
    val filePosition = remember(agent.id) { FileBrowserPosition() }
    val context = LocalContext.current
    var cameraUri by remember { mutableStateOf<Uri?>(null) }
    val imagePicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) {
        it?.let { uri -> viewModel.addAttachment(uri, "image") }
    }
    val filePicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) {
        it?.let(viewModel::addAttachment)
    }
    val camera = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { saved ->
        if (saved) cameraUri?.let { viewModel.addAttachment(it, "image") }
    }

    Scaffold(
        modifier = Modifier.fillMaxSize(),
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(agent.displayName, style = MaterialTheme.typography.titleMedium)
                        Text(
                            if (agent.currentWorkItemId != null) "有进行中的 WorkItem" else agent.statusLabel(),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                },
                navigationIcon = {
                    IconButton(onClick = { viewModel.handleSystemBack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回上一级")
                    }
                },
                actions = {
                    IconButton(onClick = viewModel::refresh, enabled = !state.busy) {
                        Icon(Icons.Default.Refresh, contentDescription = "刷新")
                    }
                },
            )
        },
    ) { padding ->
        CompositionLocalProvider(LocalOpenMessageFile provides viewModel::openMessageFile) {
        // Resize the timeline and composer together; padding only the composer leaves an IME-sized gap.
        Column(Modifier.fillMaxSize().padding(padding).imePadding()) {
            state.error?.let { ErrorBanner(it, viewModel::clearError) }
            state.statusMessage?.let { Text(it, modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            AgentSectionBar(state.agentSection, viewModel::selectAgentSection)
            BoxWithConstraints(Modifier.weight(1f)) {
                val wideWorkLayout = maxWidth >= 720.dp && state.agentSection == AgentSection.Work
                when {
                    state.planFile != null -> FileReaderScreen(
                        artifact = state.planFile,
                        title = "完整计划",
                        onBack = viewModel::closePlanFile,
                        onSave = viewModel::saveArtifactToDevice,
                        onShare = { shareArtifact(context, state.planFile) },
                    )
                    state.selectedBrief != null && state.selectedWorkItem == null -> BriefScreen(state, viewModel)
                    wideWorkLayout -> {
                        Row(Modifier.fillMaxSize()) {
                            WorkItemsScreen(state, viewModel, Modifier.width(340.dp).fillMaxHeight(), workListState)
                            Box(Modifier.width(1.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant))
                            Box(Modifier.weight(1f).fillMaxHeight()) {
                                if (state.selectedWorkItem != null) {
                                    WorkItemDetailScreen(state, viewModel, showBack = false)
                                } else {
                                    EmptyPage("选择一个 WorkItem", "目标、进度、结果与关联产物将在这里打开。")
                                }
                            }
                        }
                    }
                    state.selectedActivity != null && state.selectedTurn == null -> ActivityDetailScreen(state, viewModel)
                    state.selectedTurn != null -> TurnDetailScreen(state, viewModel)
                    state.selectedWorkItem != null -> WorkItemDetailScreen(state, viewModel)
                    else -> when (state.agentSection) {
                        AgentSection.Results -> Column(Modifier.fillMaxSize()) {
                            ConversationTimeline(state, viewModel, Modifier.weight(1f), timelinePosition)
                            Composer(
                                state = state,
                                viewModel = viewModel,
                                onImage = { imagePicker.launch(arrayOf("image/*")) },
                                onFile = { filePicker.launch(arrayOf("*/*")) },
                                onCamera = {
                                    cameraUri = createCameraUri(context)
                                    cameraUri?.let(camera::launch)
                                },
                            )
                        }
                        AgentSection.Work -> WorkItemsScreen(state, viewModel, Modifier.fillMaxSize(), workListState)
                        AgentSection.Files -> WorkspaceBrowserScreen(state, viewModel, Modifier.fillMaxSize(), filePosition)
                    }
                }
            }
        }
        }
    }
}

@Composable
private fun AgentSectionBar(selected: AgentSection, onSelect: (AgentSection) -> Unit) {
    Row(
        Modifier.fillMaxWidth().height(48.dp),
    ) {
        AgentSection.entries.forEach { section ->
            val active = section == selected
            Column(
                modifier = Modifier.weight(1f).fillMaxHeight().clickable { onSelect(section) },
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Bottom,
            ) {
                Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
                    Text(
                        section.label,
                        style = MaterialTheme.typography.labelLarge,
                        color = if (active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Box(
                    Modifier.width(30.dp).height(2.dp)
                        .background(if (active) MaterialTheme.colorScheme.primary else Color.Transparent),
                )
            }
        }
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
}

private class ConversationTimelinePosition {
    val listState = LazyListState()
    var positionedAtLatest by mutableStateOf(false)
    var previousItemCount by mutableStateOf(0)
    var previousOutboxCount by mutableStateOf(0)
    var previousOlderTurnCount by mutableStateOf(0)
}

@Composable
private fun ConversationTimeline(
    state: HolonUiState,
    viewModel: HolonViewModel,
    modifier: Modifier,
    position: ConversationTimelinePosition,
) {
    val snapshot = state.conversation
    val listState = position.listState
    val recentIds = snapshot?.turns.orEmpty().mapTo(mutableSetOf(), HolonConversationTurn::id)
    val turns = state.olderTurns.filterNot { it.id in recentIds } + snapshot?.turns.orEmpty()
    val pendingRows = if (snapshot?.pendingInputs?.isNotEmpty() == true) 1 else 0
    val historyRows = if (state.hasOlderTurns) 1 else 0
    val itemCount = pendingRows + historyRows + turns.size + state.outbox.size
    val agentId = state.selectedAgent?.id
    val latestBriefId = state.selectedAgent?.latestBrief?.briefId
    val latestBriefIndex = turns.indexOfLast { latestBriefId != null && latestBriefId in it.briefIds }
    val latestBriefLoaded = latestBriefId != null && state.briefs.containsKey(latestBriefId)
    val latestBriefRead = agentId != null && state.readBriefIds[agentId] == latestBriefId
    LaunchedEffect(agentId, latestBriefId, latestBriefIndex, latestBriefLoaded, latestBriefRead, historyRows, pendingRows) {
        if (agentId != null && latestBriefId != null && latestBriefLoaded && !latestBriefRead && latestBriefIndex >= 0) {
            val itemIndex = historyRows + pendingRows + latestBriefIndex
            snapshotFlow { listState.layoutInfo.visibleItemsInfo.any { it.index == itemIndex } }.first { it }
            viewModel.markBriefRead(agentId, latestBriefId)
        }
    }
    LaunchedEffect(snapshot?.snapshotCursor, itemCount, state.outbox.size, state.olderTurns.size) {
        if (snapshot != null && itemCount > 0) {
            val lastVisible = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index
            val nearBottom = lastVisible?.let { it >= position.previousItemCount - 2 } ?: !position.positionedAtLatest
            val justSent = state.outbox.size > position.previousOutboxCount
            val olderPageLoaded = state.olderTurns.size > position.previousOlderTurnCount
            if (!olderPageLoaded && (!position.positionedAtLatest || nearBottom || justSent)) {
                listState.animateScrollToItem(itemCount - 1)
            }
            position.positionedAtLatest = true
        }
        position.previousItemCount = itemCount
        position.previousOutboxCount = state.outbox.size
        position.previousOlderTurnCount = state.olderTurns.size
    }
    if (state.busy && snapshot == null) {
        Box(modifier.fillMaxWidth(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    LazyColumn(
        state = listState,
        modifier = modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 14.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        if (state.hasOlderTurns) item(key = "load-older-turns") {
            TextButton(onClick = viewModel::loadOlderTurns, enabled = !state.historyBusy && state.historyBeforeCursor != null) {
                if (state.historyBusy) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                else Text(if (state.historyBeforeCursor == null) "更早记录暂不可读取" else "加载更早记录")
            }
        }
        snapshot?.pendingInputs?.takeIf { it.isNotEmpty() }?.let { pending ->
            item {
                Surface(
                    color = MaterialTheme.colorScheme.primaryContainer,
                    shape = RoundedCornerShape(10.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(
                        "${pending.size} 条输入正在排队",
                        modifier = Modifier.padding(12.dp),
                        color = MaterialTheme.colorScheme.onPrimaryContainer,
                    )
                }
            }
        }
        itemsIndexed(turns, key = { _, turn -> turn.id }) { index, turn ->
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                val day = turn.startedAt?.take(10)
                val previousDay = turns.getOrNull(index - 1)?.startedAt?.take(10)
                if (day != null && day != previousDay) {
                    Text(day, modifier = Modifier.fillMaxWidth().padding(top = 8.dp), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                TurnCard(
                    turn = turn,
                    brief = turn.briefIds.firstNotNullOfOrNull(state.briefs::get),
                    onRelatedContent = viewModel::openBrief,
                    onDetail = { viewModel.openTurn(turn) },
                )
            }
        }
        items(state.outbox, key = OutboxEntity::requestId) { message ->
            LocalMessageCard(
                message,
                retryEnabled = !state.enqueueing,
                onRetry = { viewModel.retryMessage(message) },
                onEdit = { viewModel.editFailedMessage(message) },
                onRemove = { viewModel.removeFailedMessage(message) },
            )
        }
        if (turns.isEmpty() && state.outbox.isEmpty()) {
            item { EmptyPage("开始会话", "向 ${state.selectedAgent?.displayName} 说明你希望完成的工作。") }
        }
    }
}

@Composable
private fun TurnCard(
    turn: HolonConversationTurn,
    brief: run.holon.android.sdk.HolonBrief?,
    onRelatedContent: (String) -> Unit,
    onDetail: () -> Unit,
) {
    val context = LocalContext.current
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        turn.inputs.filter {
            it.presentationClass == "operator" ||
                (it.presentationClass == null && turn.presentationClass == "operator")
        }.forEach { input ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Surface(
                    color = MaterialTheme.colorScheme.primaryContainer,
                    shape = RoundedCornerShape(14.dp, 14.dp, 4.dp, 14.dp),
                    modifier = Modifier.fillMaxWidth(0.92f),
                ) {
                    Column(Modifier.padding(horizontal = 13.dp, vertical = 10.dp)) {
                        MarkdownText(input.preview.ifBlank { "已提交输入" })
                        input.createdAt?.let { Text(relativeTime(it), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onPrimaryContainer) }
                        input.actorDisplayName?.let {
                            Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onPrimaryContainer)
                        }
                    }
                }
            }
        }
        if (brief != null) {
            Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min)) {
                Box(
                    Modifier.width(2.dp).fillMaxHeight().background(MaterialTheme.colorScheme.primary),
                )
                Column(
                    Modifier.weight(1f).padding(start = 14.dp, end = 2.dp, top = 2.dp, bottom = 10.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            brief.createdAt.take(16).replace('T', ' '),
                            modifier = Modifier.weight(1f),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        turn.exceptionStatus()?.let { (label, tone) -> CompactStatus(label, tone) }
                        IconButton(onClick = { shareBrief(context, brief.text) }, modifier = Modifier.size(40.dp)) {
                            Icon(Icons.Default.Share, contentDescription = "分享结果", modifier = Modifier.size(18.dp))
                        }
                    }
                    MarkdownText(brief.text.ifBlank { "结果没有文本说明" })
                    if (brief.attachments.isNotEmpty() || brief.workItemId != null) {
                        ResultLinkRow(
                            label = "产物与关联工作",
                            meta = (brief.attachments.size + if (brief.workItemId != null) 1 else 0).toString(),
                            onClick = { onRelatedContent(brief.id) },
                        )
                    }
                    ResultLinkRow(label = "本轮过程", meta = "查看", onClick = onDetail)
                }
            }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        } else {
            Row(
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .clickable(onClick = onDetail)
                        .padding(start = 14.dp, end = 2.dp, top = 4.dp, bottom = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (turn.isRunning()) {
                    Box(
                        Modifier
                            .size(6.dp)
                            .background(MaterialTheme.colorScheme.primary.copy(alpha = 0.65f), RoundedCornerShape(50)),
                    )
                    Spacer(Modifier.width(8.dp))
                }
                turn.compactStatusText()?.let { status ->
                    Text(
                        status,
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } ?: Spacer(Modifier.weight(1f))
                Text("查看过程  ›", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
            }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        }
    }
}

@Composable
internal fun ResultLinkRow(label: String, meta: String, onClick: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, modifier = Modifier.weight(1f), style = MaterialTheme.typography.labelLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
        Text("$meta  ›", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
    }
}

internal fun HolonConversationTurn.isRunning(): Boolean =
    executionKind == "active" && completedAt == null && !settled

internal fun HolonConversationTurn.compactStatusText(): String? =
    when {
        isRunning() -> "执行中"
        terminalOutcome == "provider_failed_needs_recovery" ||
            resultKind.contains("failure", true) -> "本轮失败"
        terminalOutcome in setOf("aborted", "interrupted", "baseline_over_budget") -> "本轮未完成"
        briefIds.isNotEmpty() || resultKind == "available" -> "结果载入中"
        resultKind == "unavailable" -> "结果暂不可用"
        resultKind == "none" -> "没有结果摘要"
        else -> null
    }

private fun HolonConversationTurn.exceptionStatus(): Pair<String, StatusTone>? =
    when {
        attentionKind != null -> "需注意" to StatusTone.Warning
        resultKind.contains("failure", true) || terminalOutcome == "failure" -> "失败" to StatusTone.Danger
        else -> null
    }

@Composable
private fun TurnDetailScreen(state: HolonUiState, viewModel: HolonViewModel) {
    val turn = state.selectedTurn ?: return
    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()
    val detail = state.conversationDetail
    val activities =
        detail?.activities.orEmpty().filter {
            it.kind != "operator" && !(it.kind == "assistant" && it.summary.isBlank())
        }
    val latestActivityRevision = activities.lastOrNull()?.let { "${it.id}:${it.revision}" }
    var unseenActivities by remember(turn.id) { mutableStateOf(0) }
    var observedActivityRevision by remember(turn.id) { mutableStateOf<String?>(null) }
    var observedActivityIds by remember(turn.id) { mutableStateOf<Set<String>>(emptySet()) }
    var observedLatestActivityId by remember(turn.id) { mutableStateOf<String?>(null) }
    LaunchedEffect(turn.id, latestActivityRevision, activities.size) {
        if (activities.isNotEmpty() && latestActivityRevision != null) {
            val inputCount = turn.inputs.count { it.presentationClass != "internal" }
            val coverageCount = if (detail?.coverageKind != null && detail.coverageKind != "complete") 1 else 0
            val historyCount = if (detail?.hasMore == true) 1 else 0
            val latestIndex = 1 + inputCount + coverageCount + historyCount + activities.lastIndex
            val firstObservation = observedActivityRevision == null
            val changed = observedActivityRevision != latestActivityRevision
            val lastVisible = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            val previousLatestIndex = activities.indexOfFirst { it.id == observedLatestActivityId }
            val newlyAppended = if (previousLatestIndex < 0) 0 else activities.drop(previousLatestIndex + 1).count { it.id !in observedActivityIds }
            if (firstObservation) {
                if (turn.isRunning()) listState.scrollToItem(latestIndex)
                unseenActivities = 0
            } else if (changed && lastVisible >= latestIndex - 1) {
                listState.animateScrollToItem(latestIndex)
                unseenActivities = 0
            } else if (newlyAppended > 0) {
                unseenActivities += newlyAppended
            }
            observedActivityRevision = latestActivityRevision
            observedActivityIds = activities.mapTo(mutableSetOf(), HolonConversationActivity::id)
            observedLatestActivityId = activities.lastOrNull()?.id
        }
    }
    Box(Modifier.fillMaxSize()) {
        LazyColumn(
            state = listState,
            modifier = Modifier.fillMaxSize(),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            item {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    IconButton(onClick = viewModel::closeTurn) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回结果")
                    }
                    Column(Modifier.weight(1f)) {
                        Text("本轮过程", style = MaterialTheme.typography.headlineSmall)
                        Text(
                            if (turn.isRunning()) "实时更新" else "执行记录",
                            style = MaterialTheme.typography.labelSmall,
                            color = if (turn.isRunning()) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            turn.inputs.filter { it.presentationClass != "internal" }.forEach { input ->
                item(key = input.messageId) {
                    Surface(
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        shape = RoundedCornerShape(8.dp),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(Modifier.padding(horizontal = 13.dp, vertical = 11.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text(
                                if (input.presentationClass == "operator") "你的要求" else "触发输入",
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Text(
                                input.preview.ifBlank { "已提交输入" },
                                style = MaterialTheme.typography.bodyMedium,
                                maxLines = 8,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                }
            }
            if (state.detailBusy && state.conversationDetail == null) {
                item { Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
            }
            detail?.let {
                if (detail.hasMore) item(key = "load-older-activities") {
                    TextButton(onClick = viewModel::loadOlderActivities, enabled = !state.olderActivitiesBusy && detail.nextBeforeCursor != null) {
                        if (state.olderActivitiesBusy) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                        else Text(if (detail.nextBeforeCursor == null) "更早过程暂不可读取" else "加载更早过程")
                    }
                }
                if (detail.coverageKind != "complete") {
                    item {
                        Text(
                            detailCoverageMessage(detail.coverageKind, detail.coverageReason),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                items(activities, key = HolonConversationActivity::id) { activity ->
                    val expanded = state.selectedActivity?.id == activity.id
                    ActivityRow(
                        activity = activity,
                        expanded = expanded,
                        detail = state.selectedToolExecution.takeIf { expanded },
                        loading = expanded && state.detailBusy,
                        onOpen = {
                            if (expanded) viewModel.closeActivity() else viewModel.inspectActivity(activity)
                        },
                    )
                }
            }
            item { Spacer(Modifier.height(24.dp)) }
        }
        if (unseenActivities > 0) {
            Surface(
                modifier = Modifier.align(Alignment.BottomCenter).padding(16.dp).clickable {
                    unseenActivities = 0
                    scope.launch {
                        val lastIndex = listState.layoutInfo.totalItemsCount - 1
                        if (lastIndex >= 0) listState.animateScrollToItem(lastIndex)
                    }
                },
                color = MaterialTheme.colorScheme.primary,
                contentColor = MaterialTheme.colorScheme.onPrimary,
                shape = RoundedCornerShape(999.dp),
                shadowElevation = 4.dp,
            ) {
                Text("$unseenActivities 条新活动", modifier = Modifier.padding(horizontal = 14.dp, vertical = 9.dp))
            }
        }
    }
}

private fun detailCoverageMessage(kind: String, reason: String?): String {
    val detail =
        when (reason) {
            "retention_gap" -> "较早的执行活动已超出保留窗口"
            "legacy_ownership" -> "旧版会话无法完整关联到本轮"
            "unknown_activity_type" -> "部分执行活动暂不支持展示"
            "missing_canonical_linkage" -> "部分执行活动缺少本轮关联"
            else -> if (kind == "unavailable") "本轮执行过程不可用" else "本轮仅保留了部分执行过程"
        }
    return if (kind == "unavailable") detail else "过程可能不完整 · $detail"
}

@Composable
internal fun ActivityRow(
    activity: HolonConversationActivity,
    expanded: Boolean = false,
    detail: run.holon.android.sdk.HolonToolExecutionSnapshot? = null,
    loading: Boolean = false,
    onOpen: () -> Unit,
) {
    val isTool = activity.kind == "tool"
    var showRaw by remember(activity.id) { mutableStateOf(false) }
    Row(
        modifier = Modifier.fillMaxWidth().height(IntrinsicSize.Min),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.fillMaxHeight().width(16.dp)) {
            Box(Modifier.size(8.dp).background(if (isTool) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outline, RoundedCornerShape(99.dp)))
            Box(Modifier.width(1.dp).weight(1f).background(MaterialTheme.colorScheme.outlineVariant))
        }
        Column(Modifier.weight(1f).padding(bottom = 13.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(
                modifier = if (isTool) Modifier.fillMaxWidth().clickable(onClick = onOpen).padding(vertical = 2.dp) else Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    if (isTool) "工具调用" else "Assistant",
                    modifier = Modifier.weight(1f),
                    style = MaterialTheme.typography.labelMedium,
                    color = if (isTool) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (isTool) {
                    Text(
                        if (expanded) "收起  ⌃" else "展开  ›",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
            }
            if (isTool) {
                Text(
                    activity.summary.ifBlank { "打开查看工具输入与输出" },
                    modifier = Modifier.fillMaxWidth().clickable(onClick = onOpen).padding(vertical = 6.dp),
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            } else {
                MarkdownText(activity.summary.ifBlank { "（空消息）" })
            }
            AnimatedVisibility(visible = isTool && expanded) {
                Surface(
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    shape = RoundedCornerShape(8.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    when {
                        loading -> Box(Modifier.fillMaxWidth().padding(20.dp), contentAlignment = Alignment.Center) {
                            CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                        }
                        detail != null -> Column(
                            Modifier.fillMaxWidth().padding(12.dp),
                            verticalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    detail.toolName,
                                    modifier = Modifier.weight(1f),
                                    style = MaterialTheme.typography.labelLarge,
                                    fontFamily = FontFamily.Monospace,
                                )
                                CompactStatus(
                                    detail.status,
                                    if (detail.status in setOf("completed", "success", "succeeded")) StatusTone.Success else StatusTone.Neutral,
                                )
                            }
                            detail.summary?.takeIf { it.isNotBlank() && it != activity.summary }?.let {
                                Text(it, style = MaterialTheme.typography.bodySmall)
                            }
                            detail.raw["input"]?.let { ToolPayloadPreview("输入", it.toString()) }
                            detail.raw["output"]?.let { ToolPayloadPreview("输出", it.toString()) }
                            if (detail.artifactCount > 0) {
                                Text(
                                    "产生 ${detail.artifactCount} 个产物",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            TextButton(onClick = { showRaw = !showRaw }) {
                                Text(if (showRaw) "收起原始记录" else "查看原始记录")
                            }
                            if (showRaw) SelectionContainer {
                                Text(
                                    detail.raw.toString().take(16_000),
                                    modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                                    style = MaterialTheme.typography.bodySmall,
                                    fontFamily = FontFamily.Monospace,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                        else -> Text(
                            "没有可读取的工具输入或输出。",
                            modifier = Modifier.padding(12.dp),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ActivityDetailScreen(state: HolonUiState, viewModel: HolonViewModel) {
    val activity = state.selectedActivity ?: return
    val tool = state.selectedToolExecution
    var showRaw by remember(activity.id) { mutableStateOf(false) }
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = viewModel::closeActivity) { Text("‹ 本轮") }
                Column(Modifier.weight(1f)) {
                    Text(if (activity.kind == "tool") "工具调用" else "Assistant 文本", style = MaterialTheme.typography.headlineSmall)
                    Text(activity.id, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        item {
            HolonSection("摘要", eyebrow = activity.kind.uppercase()) {
                Text(activity.summary.ifBlank { "没有摘要" })
            }
        }
        if (state.detailBusy && tool == null) {
            item { Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
        }
        tool?.let { detail ->
            item {
                HolonSection(detail.toolName) {
                    detail.summary?.takeIf { it != activity.summary }?.let { Text(it) }
                    if (detail.artifactCount > 0) SettingsValue("产物", detail.artifactCount.toString())
                }
            }
            detail.raw["input"]?.let { input -> item { ToolPayloadPreview("输入", input.toString()) } }
            detail.raw["output"]?.let { output -> item { ToolPayloadPreview("输出", output.toString()) } }
            item { TextButton(onClick = { showRaw = !showRaw }) { Text(if (showRaw) "收起原始记录" else "查看原始记录") } }
            if (showRaw) item { ToolPayloadPreview("原始记录", detail.raw.toString(), 16_000) }
        }
    }
}

@Composable
private fun ToolPayloadPreview(title: String, payload: String, limit: Int = 2_000) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(title, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(8.dp)) {
            SelectionContainer {
                Text(
                    payload.take(limit),
                    modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(10.dp),
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                )
            }
        }
        if (payload.length > limit) Text("预览已截断", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun WorkItemsScreen(
    state: HolonUiState,
    viewModel: HolonViewModel,
    modifier: Modifier,
    listState: LazyListState,
) {
    var filter by remember(state.selectedAgent?.id) { mutableStateOf("all") }
    val currentId = state.selectedAgent?.currentWorkItemId
    val sorted = state.workItems.filter { item ->
        when (filter) {
            "open" -> item.state !in setOf("completed", "aborted", "failed")
            "completed" -> item.state == "completed"
            else -> true
        }
    }.sortedWith(
        compareByDescending<HolonWorkItemSnapshot> { it.workItemId == currentId }
            .thenBy { it.state == "completed" }
            .thenByDescending { it.updatedAt.orEmpty() },
    )
    LazyColumn(
        state = listState,
        modifier = modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 10.dp),
    ) {
        item {
            Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text("工作记录", style = MaterialTheme.typography.headlineSmall)
                Text(
                    "已加载 ${state.workItems.size} 项 · ${state.workItems.count { it.state !in setOf("completed", "aborted", "failed") }} 项进行中 · ${state.workItems.count { it.state == "completed" }} 项已完成",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
        item {
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                listOf("all" to "全部", "open" to "进行中", "completed" to "已完成").forEach { (value, label) ->
                    FilterChip(selected = filter == value, onClick = { filter = value }, label = { Text(label) })
                }
            }
        }
        if (state.workItemsBusy && sorted.isEmpty()) {
            item { Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
        }
        items(sorted, key = HolonWorkItemSnapshot::workItemId) { item ->
            val done = item.todoList.count { it.state == "completed" }
            Surface(
                color = if (item.workItemId == currentId) MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.36f) else MaterialTheme.colorScheme.background,
                modifier = Modifier.fillMaxWidth().clickable { viewModel.openWorkItem(item) },
            ) {
                Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            item.objective ?: item.workItemId,
                            modifier = Modifier.weight(1f),
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                            fontWeight = FontWeight.SemiBold,
                        )
                        CompactStatus(workItemStatusLabel(item), workItemTone(item))
                    }
                    Text(
                        item.listSummary(),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            listOfNotNull(
                                if (item.workItemId == currentId) "当前工作" else null,
                                item.updatedAt?.let(::relativeTime),
                                item.todoList.takeIf { it.isNotEmpty() }?.let { "$done/${it.size} 步" },
                            ).joinToString(" · ").ifBlank { "等待更多信息" },
                            modifier = Modifier.weight(1f),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Text("›", color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
            HorizontalDivider(modifier = Modifier.padding(start = 16.dp), color = MaterialTheme.colorScheme.outlineVariant)
        }
        if (!state.workItemsBusy && sorted.isEmpty()) item {
            EmptyPage(
                if (state.workItems.isEmpty()) "还没有工作记录" else "当前筛选没有工作记录",
                if (state.workItems.isEmpty()) "Agent 的工作计划和验收结果会显示在这里。" else "选择“全部”查看其他工作。",
            )
        }
        if (state.workItemsHasMore) item {
            TextButton(onClick = viewModel::loadMoreWorkItems, enabled = !state.workItemsLoadingMore, modifier = Modifier.fillMaxWidth()) {
                if (state.workItemsLoadingMore) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                else Text("加载更多工作记录")
            }
        }
    }
}

@Composable
private fun WorkItemDetailScreen(state: HolonUiState, viewModel: HolonViewModel, showBack: Boolean = true) {
    val item = state.selectedWorkItem ?: return
    val finished = item.state == "completed"
    val completed = item.todoList.count { it.state == "completed" }
    var showDetails by remember(item.workItemId) { mutableStateOf(false) }
    var showSteps by remember(item.workItemId, finished) { mutableStateOf(!finished) }
    var showPlan by remember(item.workItemId, finished) { mutableStateOf(!finished) }
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (showBack) {
                    IconButton(onClick = viewModel::closeWorkItem) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回工作列表")
                    }
                }
                Column(Modifier.weight(1f)) {
                    Text("工作详情", style = MaterialTheme.typography.headlineSmall)
                    if (!finished) item.focus?.takeUnless { it.equals(item.state, ignoreCase = true) }?.let {
                        Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
                CompactStatus(workItemStatusLabel(item), workItemTone(item))
            }
        }
        item {
            HolonSection("目标") {
                Text(item.objective ?: "没有目标说明", style = MaterialTheme.typography.bodyLarge)
            }
        }
        item.blockedBy?.let { blocker ->
            item { HolonSection("需要处理", eyebrow = "BLOCKED") { Text(blocker, color = MaterialTheme.colorScheme.error) } }
        }
        if (finished) {
            item {
                HolonSection("结果") {
                    item.resultSummary?.takeIf(String::isNotBlank)?.let { MarkdownText(it) }
                        ?: EmptyHint("这项工作没有独立的结果摘要。")
                    item.resultBriefId?.let { briefId ->
                        ResultLinkRow("查看关联 brief", "查看") { viewModel.openBrief(briefId) }
                    }
                    ResultLinkRow("查看 Agent 结果", "打开") { viewModel.selectAgentSection(AgentSection.Results) }
                }
            }
        }
        if (!finished && (item.focus != null || item.schedulingState != null || item.recheckAt != null)) {
            item {
                HolonSection("当前步骤") {
                    Text(item.focus ?: "等待下一次调度")
                    item.recheckAt?.let { SettingsValue("再次检查", it) }
                }
            }
        }
        if (item.todoList.isNotEmpty()) {
            item {
                if (finished) {
                    TextButton(onClick = { showSteps = !showSteps }, modifier = Modifier.fillMaxWidth()) {
                        Text(if (showSteps) "收起步骤记录" else "查看步骤记录 · $completed/${item.todoList.size}")
                    }
                }
                if (showSteps) {
                    HolonSection(if (finished) "步骤记录" else "进度", eyebrow = "$completed/${item.todoList.size}") {
                        if (!finished) LinearProgressIndicator(
                            progress = { completed.toFloat() / item.todoList.size.toFloat() },
                            modifier = Modifier.fillMaxWidth(),
                        )
                        item.todoList.forEach { todo ->
                            Row(horizontalArrangement = Arrangement.spacedBy(9.dp), verticalAlignment = Alignment.Top) {
                                Icon(
                                    imageVector = if (todo.state == "completed") Icons.Default.CheckCircle else Icons.Default.RadioButtonUnchecked,
                                    contentDescription = null,
                                    tint = if (todo.state == "completed") HolonSuccess else MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.size(18.dp),
                                )
                                Text(todo.text, modifier = Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                            }
                        }
                    }
                }
            }
        }
        if (!finished) item.resultSummary?.takeIf(String::isNotBlank)?.let { result ->
            item {
                HolonSection("结果") {
                    MarkdownText(result)
                    item.resultBriefId?.let { briefId ->
                        ResultLinkRow("查看关联 brief", "查看") { viewModel.openBrief(briefId) }
                    }
                }
            }
        }
        item.planArtifact?.let { plan ->
            item {
                if (finished) {
                    TextButton(
                        onClick = viewModel::openWorkItemPlan,
                        enabled = !state.workItemsBusy && !plan.workspaceId.isNullOrBlank() && !plan.relativePath.isNullOrBlank(),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(if (state.workItemsBusy) "正在读取计划…" else "打开完整计划")
                    }
                    TextButton(onClick = { showPlan = !showPlan }, modifier = Modifier.fillMaxWidth()) {
                        Text(if (showPlan) "收起计划预览" else "查看计划预览")
                    }
                }
                if (showPlan) HolonSection(if (finished) "计划预览" else "计划") {
                    if (!finished) {
                        TextButton(
                            onClick = viewModel::openWorkItemPlan,
                            enabled = !state.workItemsBusy && !plan.workspaceId.isNullOrBlank() && !plan.relativePath.isNullOrBlank(),
                        ) {
                            Text(if (state.workItemsBusy) "正在读取计划…" else "打开完整计划")
                        }
                    }
                    if (plan.workspaceId.isNullOrBlank() || plan.relativePath.isNullOrBlank()) {
                        Text("此 Holon 版本未提供计划文件的读取位置", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    MarkdownText(plan.preview ?: "计划文件可用，但没有内联预览。")
                    if (!plan.previewComplete) Text("此处只显示计划开头", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        if (item.workRefs.isNotEmpty()) {
            item {
                HolonSection("相关工作", eyebrow = item.workRefs.size.toString()) {
                    item.workRefs.take(20).forEach { ref ->
                        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                            Column(Modifier.weight(1f)) {
                                Text(ref.title ?: ref.ref, maxLines = 2, overflow = TextOverflow.Ellipsis)
                                Text(
                                    listOfNotNull(ref.kind, ref.status).joinToString(" · "),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                }
            }
        }
        item {
            TextButton(onClick = { showDetails = !showDetails }, modifier = Modifier.fillMaxWidth()) {
                Icon(if (showDetails) Icons.Default.ExpandLess else Icons.Default.ExpandMore, contentDescription = null)
                Spacer(Modifier.width(6.dp))
                Text(if (showDetails) "收起技术详情" else "技术详情")
            }
        }
        if (showDetails) {
            item {
                HolonSection("技术详情") {
                    SettingsValue("ID", item.workItemId)
                    SettingsValue("状态", item.state)
                    item.updatedAt?.let { SettingsValue("更新", it) }
                    item.revision?.let { SettingsValue("版本", it.toString()) }
                }
            }
        }
        if (state.workItemsBusy) item { CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp) }
    }
}

@Composable
private fun WorkspaceBrowserScreen(
    state: HolonUiState,
    viewModel: HolonViewModel,
    modifier: Modifier,
    position: FileBrowserPosition,
) {
    val context = LocalContext.current
    val prepared = state.preparedArtifact
    val path = state.workspaceDirectory?.path.orEmpty()
    LaunchedEffect(path) {
        if (position.previousPath != path) {
            position.previousPath = path
            position.search = ""
            position.listState.scrollToItem(0)
        }
    }
    val saveFile = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("*/*")) { target ->
        val source = state.preparedArtifact ?: return@rememberLauncherForActivityResult
        target?.let { uri -> viewModel.saveArtifactToDevice(source, uri) }
    }
    if (prepared != null) {
        if (isReadableTextFile(prepared.mediaType, prepared.fileName)) {
            FileReaderScreen(
                artifact = prepared,
                title = "文件",
                onBack = viewModel::returnFromMessageFile,
                backLabel = if (state.fileLinkOrigin != null) "消息" else "文件列表",
                onSave = viewModel::saveArtifactToDevice,
                onShare = { shareArtifact(context, prepared) },
            )
            return
        }
        Column(
            modifier.verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            TextButton(onClick = viewModel::returnFromMessageFile) {
                Text(if (state.fileLinkOrigin != null) "‹ 返回消息" else "‹ 返回文件列表")
            }
            Text(prepared.fileName, style = MaterialTheme.typography.headlineSmall)
            Text(prepared.mediaType, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            ArtifactPreview(prepared)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = { saveFile.launch(prepared.fileName) }) { Text("保存到设备") }
                TextButton(onClick = { shareArtifact(context, prepared) }) { Text("分享或打开") }
            }
        }
        return
    }
    val visibleEntries = state.workspaceDirectory?.entries.orEmpty()
        .filter { entry ->
            (position.showHidden || !entry.name.startsWith('.')) &&
                (position.search.isBlank() || entry.name.contains(position.search, ignoreCase = true))
        }
        .sortedWith(
            if (position.sortRecent) {
                compareBy<HolonWorkspaceEntry> { it.type != "directory" }
                    .thenByDescending { it.modified ?: Long.MIN_VALUE }
                    .thenBy { it.name.lowercase() }
            } else {
                compareBy<HolonWorkspaceEntry> { it.type != "directory" }.thenBy { it.name.lowercase() }
            },
        )
    val recentArtifacts = state.briefs.values.sortedByDescending { it.createdAt }
        .flatMap { it.attachments }
        .filter { it.uri?.startsWith("workspace://") == true }
        .distinctBy { it.uri }
        .take(3)
    LazyColumn(
        state = position.listState,
        modifier = modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 10.dp),
    ) {
        if (recentArtifacts.isNotEmpty()) {
            item {
                Text(
                    "最近产物",
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    style = MaterialTheme.typography.titleMedium,
                )
            }
            items(recentArtifacts, key = { it.uri.orEmpty() }) { attachment ->
                Box(Modifier.padding(horizontal = 16.dp)) {
                    ResultLinkRow(
                        label = attachment.name,
                        meta = "预览",
                        onClick = { attachment.uri?.let { viewModel.prepareArtifact(it, attachment.name) } },
                    )
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            }
        }
        if (state.workspaces.size > 1) {
            item {
                Row(
                    modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    state.workspaces.forEach { workspace ->
                        FilterChip(
                            selected = workspace == state.selectedWorkspace,
                            onClick = { viewModel.selectWorkspace(workspace) },
                            label = {
                                Text(
                                    "${if (workspace.isActive) "当前 · " else ""}${workspace.label}",
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            },
                        )
                    }
                }
            }
        }
        state.selectedWorkspace?.let { workspace ->
            item {
                WorkspaceBreadcrumbs(
                    path = state.workspaceDirectory?.path.orEmpty(),
                    rootLabel = workspace.label,
                    onOpen = viewModel::navigateWorkspaceTo,
                )
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            }
        }
        item {
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                OutlinedTextField(
                    value = position.search,
                    onValueChange = { position.search = it },
                    placeholder = { Text("查找当前文件夹") },
                    singleLine = true,
                    modifier = Modifier.weight(1f),
                )
                FilterChip(
                    selected = position.showHidden,
                    onClick = { position.showHidden = !position.showHidden },
                    label = { Text("隐藏文件") },
                )
                FilterChip(
                    selected = position.sortRecent,
                    onClick = { position.sortRecent = !position.sortRecent },
                    label = { Text(if (position.sortRecent) "最新" else "名称") },
                )
            }
        }
        if (state.workspaceBusy) {
            item { Box(Modifier.fillMaxWidth().padding(20.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
        }
        items(visibleEntries, key = HolonWorkspaceEntry::name) { entry ->
            Surface(
                color = MaterialTheme.colorScheme.background,
                modifier = Modifier.fillMaxWidth().clickable { viewModel.openWorkspaceEntry(entry.name, entry.type == "directory") },
            ) {
                Row(Modifier.padding(horizontal = 16.dp, vertical = 11.dp), verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        imageVector = fileIcon(entry),
                        contentDescription = null,
                        tint = if (entry.type == "directory") MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.size(24.dp),
                    )
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f)) {
                        Text(entry.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        if (entry.type != "directory") Text(
                            listOfNotNull(
                                formatBytes(entry.size),
                                entry.modified?.let(::relativeFileTime),
                            ).joinToString(" · "),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Text(if (entry.type == "directory") "›" else "", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            HorizontalDivider(modifier = Modifier.padding(start = 52.dp), color = MaterialTheme.colorScheme.outlineVariant)
        }
        if (!state.workspaceBusy && state.workspaces.isEmpty()) item { EmptyPage("没有可浏览的 workspace", "Agent 尚未连接可访问的工作区。") }
        if (!state.workspaceBusy && state.workspaces.isNotEmpty() && visibleEntries.isEmpty()) {
            item {
                EmptyPage(
                    if (state.workspaceDirectory?.entries.isNullOrEmpty()) "这个文件夹是空的" else "没有匹配的文件",
                    if (state.workspaceDirectory?.entries.isNullOrEmpty()) "返回上一级继续浏览。" else "调整搜索或显示隐藏文件。",
                )
            }
        }
        item { Spacer(Modifier.height(20.dp)) }
    }
}

private class FileBrowserPosition {
    val listState = LazyListState()
    var previousPath: String? = null
    var search by mutableStateOf("")
    var showHidden by mutableStateOf(false)
    var sortRecent by mutableStateOf(false)
}

@Composable
private fun WorkspaceBreadcrumbs(path: String, rootLabel: String, onOpen: (String) -> Unit) {
    val parts = path.trim('/').split('/').filter(String::isNotBlank)
    Row(
        modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TextButton(onClick = { onOpen("") }) {
            Text(rootLabel, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        parts.forEachIndexed { index, part ->
            Text("/", color = MaterialTheme.colorScheme.onSurfaceVariant)
            TextButton(onClick = { onOpen(parts.take(index + 1).joinToString("/")) }) {
                Text(part, maxLines = 1)
            }
        }
    }
}

@Composable
private fun LocalMessageCard(
    message: OutboxEntity,
    retryEnabled: Boolean,
    onRetry: () -> Unit,
    onEdit: () -> Unit,
    onRemove: () -> Unit,
) {
    val label =
        when (message.state) {
            "pending" -> "待发送"
            "sending" -> "发送中"
            "received" -> "已接收"
            "unknown" -> "结果未知 · 将安全重试"
            "failed" -> "发送失败"
            else -> message.state
        }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        Surface(
            color = MaterialTheme.colorScheme.primaryContainer,
            shape = RoundedCornerShape(14.dp, 14.dp, 3.dp, 14.dp),
            modifier = Modifier.fillMaxWidth(0.88f),
        ) {
            Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                Text(message.text.ifBlank { "附件" })
                Text(
                    label,
                    style = MaterialTheme.typography.labelSmall,
                    color = if (message.state == "failed") MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onPrimaryContainer,
                )
                message.error?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
                if (message.state in setOf("failed", "unknown")) {
                    Row {
                        TextButton(onClick = onRetry, enabled = retryEnabled) { Text("重试") }
                        if (message.state == "failed") {
                            TextButton(onClick = onEdit, enabled = retryEnabled) { Text("编辑") }
                            TextButton(onClick = onRemove, enabled = retryEnabled) { Text("本机移除") }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun Composer(
    state: HolonUiState,
    viewModel: HolonViewModel,
    onImage: () -> Unit,
    onFile: () -> Unit,
    onCamera: () -> Unit,
) {
    var attachmentMenuOpen by remember { mutableStateOf(false) }
    val canStop = state.selectedAgent?.currentRunId != null
    Surface(
        color = MaterialTheme.colorScheme.surface,
        tonalElevation = 2.dp,
        shadowElevation = 2.dp,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 12.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (state.stagingAttachment) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                    Text("正在准备附件…", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            if (state.attachments.isNotEmpty()) {
                Row(
                    modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    state.attachments.forEachIndexed { index, attachment ->
                        Surface(
                            color = MaterialTheme.colorScheme.surfaceVariant,
                            shape = RoundedCornerShape(10.dp),
                        ) {
                            Row(
                                modifier = Modifier.padding(start = 10.dp, end = 4.dp),
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(6.dp),
                            ) {
                                Icon(
                                    if (attachment.kind == "image") Icons.Default.Image else Icons.Default.AttachFile,
                                    contentDescription = if (attachment.kind == "image") "图片" else "文件",
                                    modifier = Modifier.size(18.dp),
                                )
                                Text(
                                    "${attachment.name} · ${formatBytes(attachment.size)}",
                                    modifier = Modifier.widthIn(max = 200.dp),
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                    style = MaterialTheme.typography.bodySmall,
                                )
                                IconButton(
                                    onClick = { viewModel.removeAttachment(index) },
                                    enabled = !state.enqueueing && !state.stagingAttachment,
                                    modifier = Modifier.size(40.dp),
                                ) {
                                    Icon(Icons.Default.Close, contentDescription = "移除 ${attachment.name}", modifier = Modifier.size(18.dp))
                                }
                            }
                        }
                    }
                }
            }
            OutlinedTextField(
                value = state.draft,
                onValueChange = viewModel::updateDraft,
                placeholder = { Text("给 Agent 发送消息…") },
                minLines = 1,
                maxLines = 6,
                enabled = !state.enqueueing,
                shape = RoundedCornerShape(16.dp),
                modifier = Modifier.fillMaxWidth(),
            )
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Box {
                    IconButton(onClick = { attachmentMenuOpen = true }, enabled = !state.enqueueing && !state.stagingAttachment) {
                        Icon(Icons.Default.Add, contentDescription = "添加附件")
                    }
                    DropdownMenu(expanded = attachmentMenuOpen, onDismissRequest = { attachmentMenuOpen = false }) {
                        DropdownMenuItem(
                            text = { Text("从相册选择") },
                            leadingIcon = { Icon(Icons.Default.PhotoLibrary, contentDescription = null) },
                            onClick = { attachmentMenuOpen = false; onImage() },
                        )
                        DropdownMenuItem(
                            text = { Text("拍照") },
                            leadingIcon = { Icon(Icons.Default.PhotoCamera, contentDescription = null) },
                            onClick = { attachmentMenuOpen = false; onCamera() },
                        )
                        DropdownMenuItem(
                            text = { Text("选择文件") },
                            leadingIcon = { Icon(Icons.Default.AttachFile, contentDescription = null) },
                            onClick = { attachmentMenuOpen = false; onFile() },
                        )
                    }
                }
                Spacer(Modifier.weight(1f))
                if (canStop) {
                    TextButton(onClick = viewModel::stopCurrentTurn, enabled = !state.abortingRun) {
                        if (state.abortingRun) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                        else Text("停止本轮")
                    }
                }
                Button(
                    onClick = viewModel::send,
                    enabled = !state.enqueueing && !state.stagingAttachment && (state.draft.isNotBlank() || state.attachments.isNotEmpty()),
                    shape = RoundedCornerShape(12.dp),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 14.dp, vertical = 10.dp),
                ) {
                    if (state.enqueueing) {
                        CircularProgressIndicator(Modifier.size(18.dp), color = MaterialTheme.colorScheme.onPrimary, strokeWidth = 2.dp)
                    } else {
                        Icon(Icons.AutoMirrored.Filled.Send, contentDescription = null, modifier = Modifier.size(18.dp))
                    }
                    Spacer(Modifier.width(6.dp))
                    Text(if (state.enqueueing) "发送中" else "发送")
                }
            }
        }
    }
}

@Composable
private fun BriefScreen(state: HolonUiState, viewModel: HolonViewModel) {
    val brief = state.selectedBrief ?: return
    val context = LocalContext.current
    val prepared = state.preparedArtifact
    val saveArtifact = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("*/*")) { target ->
        val source = state.preparedArtifact ?: return@rememberLauncherForActivityResult
        target?.let { uri -> viewModel.saveArtifactToDevice(source, uri) }
    }
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = viewModel::closeBrief) { Text("‹ 会话") }
                Column(Modifier.weight(1f)) {
                    Text("产物与关联工作", style = MaterialTheme.typography.headlineSmall)
                    Text(brief.createdAt, style = MaterialTheme.typography.labelSmall)
                }
                IconButton(onClick = { shareBrief(context, brief.text) }) {
                    Icon(Icons.Default.Share, contentDescription = "分享结果")
                }
            }
        }
        brief.workItemId?.let { id ->
            item {
                HolonSection("关联工作", eyebrow = "WORK ITEM") {
                    val item = state.workItems.firstOrNull { it.workItemId == id }
                    Text(item?.objective ?: "这项工作的详情可直接打开，不依赖工作列表是否已加载。")
                    ResultLinkRow("查看工作详情", if (state.busy) "正在读取" else "打开") {
                        viewModel.openRelatedWorkItem(id)
                    }
                }
            }
        }
        if (brief.attachments.isNotEmpty()) {
            item {
                Text("产物", style = MaterialTheme.typography.headlineSmall)
            }
            itemsIndexed(brief.attachments) { _, attachment ->
                Surface(
                    shape = RoundedCornerShape(10.dp),
                    border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                    color = MaterialTheme.colorScheme.surface,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(Modifier.padding(13.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                        Text(attachment.name, fontWeight = FontWeight.SemiBold)
                        Text(attachment.kind, style = MaterialTheme.typography.labelSmall)
                        Text(
                            when {
                                attachment.uri == null -> "此产物没有可读取 locator"
                                attachment.uri?.startsWith("workspace://") == true -> "受保护的工作区产物，可通过当前 session 读取"
                                else -> "不支持的产物 locator"
                            },
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            style = MaterialTheme.typography.bodySmall,
                        )
                        attachment.value?.let { value ->
                            Text(
                                value.toString(),
                                maxLines = 6,
                                overflow = TextOverflow.Ellipsis,
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        attachment.uri?.takeIf { it.startsWith("workspace://") }?.let { locator ->
                            OutlinedButton(
                                onClick = { viewModel.prepareArtifact(locator, attachment.name) },
                                enabled = !state.busy,
                            ) {
                                Text(if (state.busy) "正在读取…" else "预览产物")
                            }
                            if (prepared?.locator == locator) {
                                ArtifactPreview(prepared)
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    TextButton(onClick = { saveArtifact.launch(prepared.fileName) }) { Text("下载") }
                                    TextButton(onClick = { shareArtifact(context, prepared) }) { Text("分享") }
                                    TextButton(onClick = viewModel::clearPreparedArtifact) { Text("关闭预览") }
                                }
                            }
                        }
                    }
                }
            }
        }
        item { Spacer(Modifier.height(18.dp)) }
    }
}

private fun shareBrief(context: Context, text: String) {
    val intent = Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_TEXT, text)
    }
    context.startActivity(Intent.createChooser(intent, "分享结果").addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
}

@Composable
private fun ArtifactPreview(artifact: PreparedArtifact) {
    val file = remember(artifact.localPath) { File(artifact.localPath) }
    when {
        artifact.mediaType.startsWith("image/") -> {
            val bitmap by produceState<Result<android.graphics.Bitmap?>?>(null, artifact.localPath) {
                value = withContext(Dispatchers.IO) { runCatching { decodeSampledPreview(file) } }
            }
            if (bitmap?.getOrNull() != null) {
                var expanded by remember(artifact.localPath) { mutableStateOf(false) }
                val image = bitmap!!.getOrThrow()!!.asImageBitmap()
                Image(
                    bitmap = image,
                    contentDescription = "${artifact.fileName}，点按放大",
                    modifier = Modifier.fillMaxWidth().height(240.dp).clickable { expanded = true },
                )
                if (expanded) {
                    var scale by remember { mutableFloatStateOf(1f) }
                    var pan by remember { mutableStateOf(Offset.Zero) }
                    Dialog(
                        onDismissRequest = { expanded = false },
                        properties = DialogProperties(usePlatformDefaultWidth = false),
                    ) {
                        Box(Modifier.fillMaxSize().background(Color.Black)) {
                            Image(
                                bitmap = image,
                                contentDescription = artifact.fileName,
                                modifier = Modifier.fillMaxSize()
                                    .pointerInput(Unit) {
                                        detectTransformGestures { _, drag, zoom, _ ->
                                            scale = (scale * zoom).coerceIn(1f, 4f)
                                            pan = if (scale == 1f) Offset.Zero else pan + drag
                                        }
                                    }
                                    .graphicsLayer(
                                        scaleX = scale,
                                        scaleY = scale,
                                        translationX = pan.x,
                                        translationY = pan.y,
                                    ),
                            )
                            IconButton(
                                onClick = { expanded = false },
                                modifier = Modifier.align(Alignment.TopEnd).padding(16.dp),
                            ) { Icon(Icons.Default.Close, contentDescription = "关闭图片预览", tint = Color.White) }
                        }
                    }
                }
            } else {
                EmptyHint(if (bitmap == null) "正在读取图片…" else "图片无法预览，可保存或分享后打开")
            }
        }
        artifact.mediaType.startsWith("text/") || artifact.mediaType == "application/json" -> {
            val preview by produceState<TextArtifactPreview?>(null, artifact.localPath) {
                value = withContext(Dispatchers.IO) { readTextPreview(file) }
            }
            if (preview == null) {
                EmptyHint("正在读取文本预览…")
            } else {
                val text = preview!!
                if (artifact.mediaType == "text/markdown" || artifact.fileName.endsWith(".md", ignoreCase = true)) {
                    MarkdownText(text.body)
                } else {
                    Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(8.dp)) {
                        Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(12.dp)) {
                            SelectionContainer {
                                Text(text.body, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
                            }
                        }
                    }
                }
                if (text.truncated) Text("仅显示前 20,000 个字符；保存文件可查看完整内容", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        else -> EmptyHint("${artifact.mediaType} 不支持内置预览，可下载或分享后打开")
    }
}

private data class TextArtifactPreview(val body: String, val truncated: Boolean)

private fun readTextPreview(file: File): TextArtifactPreview =
    try {
        file.bufferedReader().use { reader ->
            val buffer = CharArray(20_001)
            var count = 0
            while (count < buffer.size) {
                val read = reader.read(buffer, count, buffer.size - count)
                if (read < 0) break
                count += read
            }
            TextArtifactPreview(String(buffer, 0, count.coerceAtMost(20_000)), count > 20_000)
        }
    } catch (_: IOException) {
        TextArtifactPreview("无法读取文本预览，请重新读取或保存后打开", false)
    }

private fun decodeSampledPreview(file: File): android.graphics.Bitmap? =
    BitmapFactory.Options().run {
        inJustDecodeBounds = true
        BitmapFactory.decodeFile(file.absolutePath, this)
        var sample = 1
        while (maxOf(outWidth, outHeight) / sample > 2_048) sample *= 2
        BitmapFactory.decodeFile(file.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
    }

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
                message,
                modifier = Modifier.weight(1f),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onErrorContainer,
            )
            onDismiss?.let {
                TextButton(
                    onClick = it,
                    colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.onErrorContainer),
                ) {
                    Text("关闭")
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

private fun AgentSummary.isActive(): Boolean =
    schedulingPosture in setOf("active_turn", "has_queued_input", "has_runnable_work") ||
        runtimeStatus.lowercase() in setOf("running", "active")

private fun plainTextPreview(markdown: String): String =
    markdown
        .replace(Regex("\\[([^]]+)]\\([^)]*\\)"), "${'$'}1")
        .replace(Regex("(?m)^\\s{0,3}#{1,6}\\s+"), "")
        .replace(Regex("[`*_~>]"), "")
        .replace(Regex("\\s+"), " ")
        .trim()

private fun relativeTime(value: String): String {
    val duration = runCatching { Duration.between(Instant.parse(value), Instant.now()) }.getOrNull() ?: return value
    val seconds = duration.seconds.coerceAtLeast(0)
    return when {
        seconds < 60 -> "刚刚"
        seconds < 3_600 -> "${seconds / 60} 分钟前"
        seconds < 86_400 -> "${seconds / 3_600} 小时前"
        seconds < 604_800 -> "${seconds / 86_400} 天前"
        else -> value.take(10)
    }
}

private fun syncClock(epochMillis: Long): String =
    Instant.ofEpochMilli(epochMillis).atZone(ZoneId.systemDefault()).format(DateTimeFormatter.ofPattern("HH:mm"))

private fun relativeFileTime(value: Long): String {
    val epochMillis = if (value > 10_000_000_000L) value else value * 1_000L
    val duration = Duration.between(Instant.ofEpochMilli(epochMillis), Instant.now())
    val seconds = duration.seconds.coerceAtLeast(0)
    return when {
        seconds < 60 -> "刚刚"
        seconds < 3_600 -> "${seconds / 60} 分钟前"
        seconds < 86_400 -> "${seconds / 3_600} 小时前"
        seconds < 604_800 -> "${seconds / 86_400} 天前"
        else -> "${seconds / 604_800} 周前"
    }
}

private fun workItemStatusLabel(item: HolonWorkItemSnapshot): String =
    when {
        item.blockedBy != null -> "受阻"
        item.state == "completed" -> "已完成"
        item.readiness == "ready" -> "可继续"
        item.schedulingState == "waiting" -> "等待中"
        else -> item.readiness ?: item.state
    }

private fun HolonWorkItemSnapshot.listSummary(): String =
    if (state == "completed") {
        resultSummary?.takeIf(String::isNotBlank)?.let(::plainTextPreview) ?: "查看工作详情"
    } else {
        focus
            ?.takeUnless {
                it.equals(state, ignoreCase = true) ||
                    it.equals(readiness, ignoreCase = true) ||
                    it.equals(schedulingState, ignoreCase = true)
            }
            ?: resultSummary?.takeIf(String::isNotBlank)?.let(::plainTextPreview)
            ?: "等待更多信息"
    }

private fun workItemTone(item: HolonWorkItemSnapshot): StatusTone =
    when {
        item.blockedBy != null -> StatusTone.Danger
        item.state == "completed" -> StatusTone.Success
        item.schedulingState == "waiting" -> StatusTone.Neutral
        else -> StatusTone.Accent
    }

private fun fileIcon(entry: HolonWorkspaceEntry): ImageVector =
    when {
        entry.type == "directory" -> Icons.Default.Folder
        entry.mediaType?.startsWith("image/") == true -> Icons.Default.Image
        entry.mediaType?.startsWith("text/") == true || entry.name.endsWith(".md", true) -> Icons.Default.Description
        else -> Icons.AutoMirrored.Filled.InsertDriveFile
    }

private fun AgentSummary.statusTone(): StatusTone =
    when {
        needsReply() -> StatusTone.Warning
        schedulingPosture == "blocked" -> StatusTone.Danger
        schedulingPosture == "active_turn" || runtimeStatus.lowercase() in setOf("running", "active") -> StatusTone.Accent
        schedulingPosture in setOf("has_queued_input", "has_runnable_work") -> StatusTone.Accent
        else -> StatusTone.Neutral
    }

private fun AgentSummary.statusLabel(): String =
    when {
        needsReply() -> "等你回应"
        schedulingPosture == "active_turn" -> "工作中"
        schedulingPosture == "has_queued_input" -> "已排队"
        schedulingPosture == "has_runnable_work" -> "待运行"
        schedulingPosture == "waiting_for_external" -> "等外部变化"
        schedulingPosture == "waiting_for_task" -> "等任务结果"
        schedulingPosture == "blocked" -> "受阻"
        schedulingPosture == "idle" -> "空闲"
        runtimeStatus.lowercase() == "offline" -> "离线缓存"
        else -> runtimeStatus
    }

@Composable
private fun toneColor(tone: StatusTone): Color =
    when (tone) {
        StatusTone.Neutral -> MaterialTheme.colorScheme.onSurfaceVariant
        StatusTone.Accent -> MaterialTheme.colorScheme.primary
        StatusTone.Success -> HolonSuccess
        StatusTone.Warning -> HolonWarning
        StatusTone.Danger -> MaterialTheme.colorScheme.error
    }

private fun createCameraUri(context: Context): Uri {
    val directory = File(context.cacheDir, "camera").apply { mkdirs() }
    val file = File(directory, "${UUID.randomUUID()}.jpg")
    return FileProvider.getUriForFile(context, "${context.packageName}.files", file)
}

private fun shareArtifact(context: Context, artifact: PreparedArtifact) {
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
    context.startActivity(Intent.createChooser(intent, "分享 ${artifact.fileName}").addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
}

private fun formatBytes(value: Long): String =
    when {
        value >= 1024 * 1024 -> "%.1f MB".format(value / 1024.0 / 1024.0)
        value >= 1024 -> "%.1f KB".format(value / 1024.0)
        else -> "$value B"
    }
