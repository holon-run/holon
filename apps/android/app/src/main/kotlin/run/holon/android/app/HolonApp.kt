@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import android.app.Activity
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
import androidx.compose.material.icons.filled.ContentCopy
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
import androidx.compose.foundation.interaction.collectIsDraggedAsState
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
import androidx.compose.foundation.layout.heightIn
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
import androidx.compose.material3.ModalBottomSheet
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
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
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
            AppPhase.AddingNetwork -> LoginScreen(state, viewModel, addingNetwork = true)
            AppPhase.Ready -> MainShell(state, viewModel)
        }
    }
    if (state.phase == AppPhase.Ready && state.pendingShare != null) {
        ShareToAgentDialog(state, viewModel)
    }
}

@Composable
private fun ShareToAgentDialog(state: HolonUiState, viewModel: HolonViewModel) {
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

internal fun shouldShowSavedNetworks(addingNetwork: Boolean, profiles: List<NetworkProfile>): Boolean =
    !addingNetwork && profiles.isNotEmpty()

@Composable
private fun StartingScreen() {
    Box(
        Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(16.dp)) {
            HolonMark()
            CircularProgressIndicator(modifier = Modifier.size(24.dp), strokeWidth = 2.dp)
            Text(ui("正在恢复 Holon…"), color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun LoginScreen(state: HolonUiState, viewModel: HolonViewModel, addingNetwork: Boolean = false) {
    val isInsecureHttp = state.baseUrl.trim().startsWith("http://", ignoreCase = true)
    val context = LocalContext.current
    val scanner = remember(context) { GmsBarcodeScanning.getClient(context) }
    var showLanguagePicker by remember { mutableStateOf(false) }
    if (showLanguagePicker) AppLanguagePicker { showLanguagePicker = false }
    state.pendingPairing?.let { pairing ->
        AlertDialog(
            onDismissRequest = viewModel::cancelPairing,
            title = { Text(ui("连接到这台 Holon？")) },
            text = {
                Text(
                    ui("目标地址：${pairing.address}\n") +
                        if (pairing.address.startsWith("http://")) {
                            ui("HTTP 不加密，配对票据和会话可能被同一网络上的其他人截获。仅在可信局域网中继续；推荐使用 Tailscale HTTPS。")
                        } else {
                            ui("确认这是你信任的 Holon 主机。配对码只可使用一次。")
                        },
                )
            },
            confirmButton = { TextButton(onClick = viewModel::confirmPairing) { Text(ui("确认并连接")) } },
            dismissButton = { TextButton(onClick = viewModel::cancelPairing) { Text(ui("取消")) } },
        )
    }
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
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
            if (addingNetwork) {
                IconButton(onClick = viewModel::cancelAddNetwork, enabled = !state.busy) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = ui("返回设置"))
                }
            } else {
                Spacer(Modifier.size(48.dp))
            }
            TextButton(onClick = { showLanguagePicker = true }) { Text(ui("语言")) }
        }
        Column(
            modifier = Modifier.fillMaxWidth().padding(vertical = 24.dp),
            verticalArrangement = Arrangement.spacedBy(18.dp),
        ) {
            if (state.pendingShare != null) {
                Text(ui("登录后可选择 Agent 完成分享。"), color = MaterialTheme.colorScheme.primary)
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                HolonMark()
                Column {
                    Text(ui(if (addingNetwork) "添加网络" else "连接 Holon"), style = MaterialTheme.typography.headlineLarge)
                    Text(ui(if (addingNetwork) "连接另一台 Holon 主机" else "继续你正在进行的工作"), color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            Spacer(Modifier.height(4.dp))
            Text(
                ui(if (addingNetwork) "连接成功后切换到新网络，原网络会保留在此设备上。" else "需要一台已运行的 Holon 主机，以及该主机提供的访问令牌。"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (shouldShowSavedNetworks(addingNetwork, state.networkProfiles)) {
                Text(ui("已保存的网络"), style = MaterialTheme.typography.titleSmall)
                state.networkProfiles.forEach { profile ->
                    OutlinedButton(
                        onClick = { viewModel.switchNetwork(profile.networkId) },
                        enabled = !state.busy,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.Start) {
                            Text(profile.displayName, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            Text(profile.baseUrl, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.labelSmall)
                        }
                    }
                }
            }
            OutlinedTextField(
                value = state.baseUrl,
                onValueChange = viewModel::setBaseUrl,
                label = { Text(ui("Holon 地址")) },
                supportingText = {
                    if (isInsecureHttp) {
                        Text(
                            ui("HTTP 本身不加密；请只在可信局域网或 Tailscale 等加密隧道中使用"),
                            color = MaterialTheme.colorScheme.tertiary,
                        )
                    } else {
                        Text(ui("例如 https://holon.example.com 或 http://100.64.0.1:7878"))
                    }
                },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
                singleLine = true,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedButton(
                onClick = {
                    scanner.startScan()
                        .addOnSuccessListener { barcode ->
                            viewModel.applyScannedAddress(barcode.rawValue.orEmpty())
                        }
                        .addOnFailureListener {
                            viewModel.reportScanFailure()
                        }
                },
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(ui("扫描连接二维码"))
            }
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
                        ui("我确认此地址位于可信网络或加密隧道中"),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
            OutlinedTextField(
                value = state.token,
                onValueChange = viewModel::setToken,
                label = { Text(ui("访问令牌（token）")) },
                supportingText = { Text(ui("登录后只保存可撤销的会话，不保存原始令牌")) },
                trailingIcon = {
                    TextButton(onClick = viewModel::toggleToken) {
                        Text(if (state.showToken) ui("隐藏") else ui("显示"))
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
                Text(if (state.busy) ui(if (addingNetwork) "正在连接" else "正在登录") else ui(if (addingNetwork) "添加并切换" else "登录"))
            }
            Text(
                ui("HTTPS 默认安全；HTTP 需要确认。扫码可填写地址，登录仍需 token。"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun MainShell(state: HolonUiState, viewModel: HolonViewModel) {
    val savedPages = rememberSaveableStateHolder()
    savedPages.SaveableStateProvider("${state.session?.scopeKey}:${state.selectedAgent?.id ?: state.mainDestination.name}") {
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
                            EmptyPage(ui("选择一个 Agent"), ui("结果、工作和文件将在这里打开。"))
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
}

private enum class AgentFilter(private val sourceLabel: String) {
    All("全部"),
    Attention("需回应"),
    NewResults("新结果"),
    Active("工作中"),
    ;

    val label: String get() = ui(sourceLabel)
}

@Composable
private fun AgentsScreen(state: HolonUiState, viewModel: HolonViewModel) {
    var filter by rememberSaveable { mutableStateOf(AgentFilter.All) }
    var searchOpen by rememberSaveable { mutableStateOf(state.search.isNotBlank()) }
    var networkChooser by remember { mutableStateOf(false) }
    val listState = rememberLazyListState()
    val attentionCount = state.agents.count { it.needsReply() }
    val unreadCount =
        if (state.briefReadStatesLoaded) state.agents.count { it.unreadCount(state.briefReadStates) > 0 }
        else if (state.readBriefsLoaded) state.agents.count { it.hasUnreadBrief(state.readBriefIds) }
        else 0
    val activeCount = state.agents.count { it.isActive() }
    val filtered = state.filteredAgents.filter { agent ->
        when (filter) {
            AgentFilter.All -> true
            AgentFilter.Attention -> agent.needsReply()
            AgentFilter.NewResults ->
                (state.briefReadStatesLoaded && agent.unreadCount(state.briefReadStates) > 0) ||
                    (!state.briefReadStatesLoaded && state.readBriefsLoaded && agent.hasUnreadBrief(state.readBriefIds))
            AgentFilter.Active -> agent.isActive()
        }
    }
    Scaffold(
        modifier = Modifier.fillMaxSize(),
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = {
                    Column(Modifier.clickable { networkChooser = true }) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(state.networkProfiles.firstOrNull { it.networkId == state.session?.networkId }?.displayName ?: "Holon", style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                            Icon(Icons.Default.ExpandMore, contentDescription = ui("切换网络"), modifier = Modifier.size(20.dp))
                        }
                        Text(
                            ui("${state.agents.size} 个 Agent · ${if (state.online && state.error == null) "已同步" else "离线缓存"}") +
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
                        Icon(if (searchOpen) Icons.Default.Close else Icons.Default.Search, contentDescription = if (searchOpen) ui("关闭搜索") else ui("搜索"))
                    }
                    IconButton(onClick = { viewModel.selectMainDestination(MainDestination.Settings) }) {
                        Icon(Icons.Default.Settings, contentDescription = ui("设置"))
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            state.statusMessage?.let { Text(ui(it), modifier = Modifier.padding(horizontal = 16.dp), style = MaterialTheme.typography.bodySmall) }
            state.error?.let { ErrorBanner(it, null) }
            AnimatedVisibility(searchOpen) {
                OutlinedTextField(
                    value = state.search,
                    onValueChange = viewModel::setSearch,
                    placeholder = { Text(ui("搜索名称或 ID")) },
                    leadingIcon = { Icon(Icons.Default.Search, contentDescription = null) },
                    trailingIcon = {
                        if (state.search.isNotBlank()) IconButton(onClick = { viewModel.setSearch("") }) {
                            Icon(Icons.Default.Close, contentDescription = ui("清除"))
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
            LazyColumn(state = listState, modifier = Modifier.fillMaxSize()) {
                items(filtered, key = AgentSummary::id) { agent ->
                    AgentConversationRow(
                        agent,
                        unreadCount =
                            if (state.briefReadStatesLoaded) agent.unreadCount(state.briefReadStates)
                            else if (state.readBriefsLoaded && agent.hasUnreadBrief(state.readBriefIds)) 1
                            else 0,
                        onClick = { viewModel.openAgent(agent) },
                    )
                    HorizontalDivider(modifier = Modifier.padding(start = 16.dp), color = MaterialTheme.colorScheme.outlineVariant)
                }
                if (filtered.isEmpty()) item {
                    when {
                        state.agents.isEmpty() -> EmptyPage(ui("还没有 Agent"), ui("连接成功后，Agent 会显示在这里。"))
                        filter == AgentFilter.Attention && attentionCount == 0 ->
                            EmptyPage(ui("目前没有需要回应的 Agent"), ui("选择“全部”查看其他 Agent。"))
                        filter == AgentFilter.NewResults && unreadCount == 0 ->
                            EmptyPage(ui("目前没有未读结果"), ui("选择“全部”查看其他 Agent。"))
                        filter == AgentFilter.Active && activeCount == 0 ->
                            EmptyPage(ui("目前没有工作中的 Agent"), ui("选择“全部”查看其他 Agent。"))
                        state.search.isNotBlank() ->
                            EmptyPage(ui("没有匹配的 Agent"), ui("调整搜索词或筛选条件后再试。"))
                        else -> EmptyPage(ui("这个筛选暂无 Agent"), ui("选择“全部”查看所有 Agent。"))
                    }
                }
                item { Spacer(Modifier.height(20.dp)) }
            }
        }
    }
    if (networkChooser) ModalBottomSheet(onDismissRequest = { networkChooser = false }) {
        Text(ui("网络"), style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(16.dp))
        state.networkProfiles.forEach { profile ->
            Row(Modifier.fillMaxWidth().clickable(enabled = !state.busy && !state.enqueueing) {
                networkChooser = false
                viewModel.switchNetwork(profile.networkId)
            }.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(profile.displayName, style = MaterialTheme.typography.titleSmall)
                    Text(profile.baseUrl, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                if (profile.networkId == state.session?.networkId) Icon(Icons.Default.CheckCircle, contentDescription = ui("当前网络"), tint = MaterialTheme.colorScheme.primary)
            }
        }
        TextButton(onClick = { networkChooser = false; viewModel.beginAddNetwork() }, modifier = Modifier.padding(8.dp)) { Text(ui("添加网络")) }
    }
}

@Composable
internal fun AgentConversationRow(
    agent: AgentSummary,
    compact: Boolean = false,
    unreadCount: Int = 0,
    onClick: () -> Unit,
) {
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
                    agent.needsReply() -> ui("正在等你回应")
                    briefPreview.isNotBlank() -> briefPreview
                    postureReason.isNotBlank() -> plainTextPreview(postureReason)
                    else -> ui("暂无新的工作摘要")
                },
                maxLines = if (compact) 1 else 2,
                overflow = TextOverflow.Ellipsis,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodyMedium,
            )
            if (!compact) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    if (unreadCount > 0) {
                        Text(
                            if (unreadCount == 1) ui("新结果") else ui("$unreadCount 个未读结果"),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                        )
                    }
                    Text(
                        listOfNotNull(
                            agent.latestBrief?.createdAt?.let(::relativeTime),
                            agent.currentWorkItemId?.let { ui("有进行中的 WorkItem") },
                        ).joinToString(" · ").ifBlank { ui("尚无活动") },
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
    var showLanguagePicker by remember { mutableStateOf(false) }
    var pendingSignOut by remember { mutableStateOf<String?>(null) }
    if (showLanguagePicker) AppLanguagePicker { showLanguagePicker = false }
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
                    busy = state.busy,
                    switchingNetworkId = state.switchingNetworkId,
                    onSwitch = viewModel::switchNetwork,
                    onAdd = viewModel::beginAddNetwork,
                )
            }
            item {
                HolonSection(ui("当前身份")) {
                    SettingsValue(ui("用户"), state.session?.user?.displayName ?: state.session?.user?.userId.orEmpty())
                    Text(ui("访问令牌不保存在设备上。"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            item {
                HolonSection(ui("关于")) {
                    SettingsValue(ui("App"), BuildConfig.VERSION_NAME)
                    Text(ui("连接已有 Holon 主机的移动工作台。打开应用后同步最新状态。"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
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
                    val traceScope = state.session?.networkId?.let(TraceScope::Network) ?: TraceScope.Global
                    val traceSummary = viewModel.traceRecorder.summary(traceScope)
                    SettingsValue(
                        ui("Trace"),
                        "${traceSummary.eventCount} ${ui("条")} · ${traceSummary.bytes} B",
                    )
                    TextButton(onClick = { shareTrace(context, viewModel.traceRecorder, traceScope) }) {
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
) {
    HolonSection(ui("网络")) {
        profiles.sortedByDescending { it.networkId == currentNetworkId }.forEach { profile ->
            val isCurrent = profile.networkId == currentNetworkId
            Surface(
                modifier = Modifier.fillMaxWidth().clickable(enabled = !busy && !isCurrent) {
                    onSwitch(profile.networkId)
                },
                shape = RoundedCornerShape(10.dp),
                border = BorderStroke(1.dp, if (isCurrent) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outlineVariant),
                color = MaterialTheme.colorScheme.surface,
            ) {
                Row(Modifier.fillMaxWidth().padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
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
                        Spacer(Modifier.width(8.dp))
                        Icon(Icons.Default.CheckCircle, contentDescription = ui("当前网络"), tint = MaterialTheme.colorScheme.primary)
                    }
                }
            }
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
private fun AppLanguagePicker(onDismiss: () -> Unit) {
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

private fun shareDiagnostics(context: Context, state: HolonUiState) {
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

private fun shareTrace(context: Context, recorder: TraceRecorder, scope: TraceScope) {
    val file = recorder.export(scope)
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
private fun SettingsValue(label: String, value: String) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Text(label, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.width(72.dp))
        Text(value.ifBlank { "—" }, modifier = Modifier.weight(1f))
    }
}

@Composable
private fun ConversationScreen(state: HolonUiState, viewModel: HolonViewModel) {
    val agent = state.selectedAgent ?: return
    val timelinePosition = rememberSaveable(agent.id, saver = ConversationTimelinePosition.Saver) { ConversationTimelinePosition() }
    val workListState = rememberLazyListState()
    val filePosition = remember(agent.id) { FileBrowserPosition() }
    var autoExpandedTurn by rememberSaveable { mutableStateOf<String?>(null) }
    val runningTurn = state.conversation?.turns?.lastOrNull()?.takeIf { it.isRunning() }
    LaunchedEffect(runningTurn?.id) {
        if (runningTurn != null && autoExpandedTurn != runningTurn.id) {
            autoExpandedTurn = runningTurn.id
            if (state.selectedTurn == null) viewModel.openTurn(runningTurn)
        }
    }
    LaunchedEffect(state.online) {
        if (state.online) viewModel.ensureBriefs(state.briefLoads.filterValues { it is BriefLoadState.Failed }.keys.toList(), retry = true)
    }
    var agentChooser by remember { mutableStateOf(false) }
    var modelChooser by remember(agent.id) { mutableStateOf(false) }
    var agentSearch by remember { mutableStateOf("") }
    val isDetail = state.planFile != null || state.preparedArtifact != null || state.selectedWorkItem != null || state.selectedBrief != null || state.fullScreenTurn
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
            if (!isDetail) {
            TopAppBar(
                title = {
                    Column(Modifier.clickable { agentChooser = true }) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(if (state.agentSection == AgentSection.Results) agent.displayName else state.agentSection.label, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                            Icon(Icons.Default.ExpandMore, contentDescription = ui("切换 Agent"), modifier = Modifier.size(18.dp))
                        }
                        if (state.agentSection != AgentSection.Results || agent.needsReply()) Text(if (state.agentSection != AgentSection.Results) agent.displayName else agent.statusLabel(), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
                    }
                },
                navigationIcon = {
                    IconButton(onClick = { viewModel.handleSystemBack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = ui("返回上一级"))
                    }
                },
                actions = {
                    TextButton(onClick = { modelChooser = true; viewModel.loadModelCatalog() }) {
                        Text(ui("模型"), maxLines = 1)
                    }
                    IconButton(onClick = { viewModel.selectAgentSection(AgentSection.Work) }) {
                        Icon(Icons.Default.Description, contentDescription = ui("工作记录"))
                    }
                    IconButton(onClick = { viewModel.selectAgentSection(AgentSection.Files) }) {
                        Icon(Icons.Default.Folder, contentDescription = ui("文件"))
                    }
                },
            )
            }
        },
    ) { padding ->
        CompositionLocalProvider(LocalOpenMessageFile provides viewModel::openMessageFile) {
        // Resize the timeline and composer together; padding only the composer leaves an IME-sized gap.
        Column(Modifier.fillMaxSize().padding(padding).then(if (isDetail) Modifier.statusBarsPadding() else Modifier).imePadding()) {
            state.error?.let { ErrorBanner(it, viewModel::clearError) }
            state.statusMessage?.let { Text(ui(it), modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            if (state.busy && state.conversation != null) LinearProgressIndicator(Modifier.fillMaxWidth())
            BoxWithConstraints(Modifier.weight(1f)) {
                val wideWorkLayout = maxWidth >= 720.dp && state.agentSection == AgentSection.Work
                when {
                    state.planFile != null -> FileReaderScreen(
                        artifact = state.planFile,
                        title = ui("完整计划"),
                        onBack = viewModel::closePlanFile,
                        onSave = viewModel::saveArtifactToDevice,
                        onShare = { shareArtifact(context, state.planFile) },
                    )
                    state.preparedArtifact != null -> WorkspaceBrowserScreen(state, viewModel, Modifier.fillMaxSize(), filePosition)
                    state.selectedBrief != null && state.selectedWorkItem == null -> BriefScreen(state, viewModel)
                    wideWorkLayout -> {
                        Row(Modifier.fillMaxSize()) {
                            WorkItemsScreen(state, viewModel, Modifier.width(340.dp).fillMaxHeight(), workListState)
                            Box(Modifier.width(1.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant))
                            Box(Modifier.weight(1f).fillMaxHeight()) {
                                if (state.selectedWorkItem != null) {
                                    WorkItemDetailScreen(state, viewModel, showBack = false)
                                } else {
                                    EmptyPage(ui("选择一个 WorkItem"), ui("目标、进度、结果与关联产物将在这里打开。"))
                                }
                            }
                        }
                    }
                    state.selectedActivity != null && state.selectedTurn == null -> ActivityDetailScreen(state, viewModel)
                    state.selectedTurn != null && state.fullScreenTurn -> TurnDetailScreen(state, viewModel)
                    state.selectedWorkItem != null -> WorkItemDetailScreen(state, viewModel)
                    else -> when (state.agentSection) {
                        AgentSection.Results -> Column(Modifier.fillMaxSize()) {
                            state.workItems.firstOrNull { it.workItemId == agent.currentWorkItemId }?.let { work ->
                                Row(Modifier.fillMaxWidth().clickable { viewModel.openRelatedWorkItem(work.workItemId) }.padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                                    Icon(Icons.Default.Description, contentDescription = null, modifier = Modifier.size(16.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                                    Spacer(Modifier.width(8.dp))
                                    Text(work.objective ?: ui("当前工作"), style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                                }
                            }
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
    if (agentChooser) ModalBottomSheet(onDismissRequest = { agentChooser = false }) {
        OutlinedTextField(value = agentSearch, onValueChange = { agentSearch = it }, placeholder = { Text(ui("搜索名称或 ID")) }, singleLine = true, modifier = Modifier.fillMaxWidth().padding(16.dp))
        LazyColumn {
            items(state.recentAgents.filter { it.displayName.contains(agentSearch, true) || it.id.contains(agentSearch, true) }, key = AgentSummary::id) { other ->
                AgentConversationRow(other, unreadCount = other.unreadCount(state.briefReadStates), onClick = { agentChooser = false; viewModel.openAgent(other) })
            }
        }
    }
    if (modelChooser) ModelPickerSheet(
        state = state,
        onDismiss = { modelChooser = false },
        onRefresh = { viewModel.loadModelCatalog(refresh = true) },
        onSelect = { model, effort -> viewModel.setAgentModel(model, effort) },
        onAuto = viewModel::clearAgentModel,
    )
}


private class ConversationTimelinePosition(val listState: LazyListState = LazyListState()) {
    var positionedAtLatest by mutableStateOf(false)
    var followLatest by mutableStateOf(true)
    var previousOutboxCount by mutableStateOf(0)
    var pendingAnchor by mutableStateOf<String?>(null)
    var pendingOffset = 0

    companion object {
        val Saver = listSaver<ConversationTimelinePosition, Any>(
            save = { position ->
                val list = position.listState
                listOf(
                    position.pendingAnchor ?: list.layoutInfo.visibleItemsInfo.firstOrNull { it.index == list.firstVisibleItemIndex }?.key?.toString().orEmpty(),
                    list.firstVisibleItemIndex,
                    if (position.pendingAnchor != null) position.pendingOffset else list.firstVisibleItemScrollOffset,
                    position.positionedAtLatest,
                    position.followLatest,
                )
            },
            restore = { saved ->
                ConversationTimelinePosition(LazyListState(saved[1] as Int, saved[2] as Int)).apply {
                    positionedAtLatest = saved[3] as Boolean
                    followLatest = saved[4] as Boolean
                    pendingAnchor = (saved[0] as String).takeIf { it.isNotBlank() && !followLatest }
                    pendingOffset = saved[2] as Int
                }
            },
        )
    }
}

@Composable
private fun ModelPickerSheet(
    state: HolonUiState,
    onDismiss: () -> Unit,
    onRefresh: () -> Unit,
    onSelect: (String, String?) -> Unit,
    onAuto: () -> Unit,
) {
    val agent = state.selectedAgent ?: return
    var search by remember(agent.id) { mutableStateOf("") }
    var pendingModel by remember(agent.id) { mutableStateOf<String?>(null) }
    var effort by remember(agent.id, pendingModel) { mutableStateOf<String?>(null) }
    val options = state.modelCatalog?.options.orEmpty()
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            Modifier.fillMaxWidth().padding(horizontal = 20.dp).navigationBarsPadding(),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(ui("选择模型"), style = MaterialTheme.typography.titleLarge)
                    Text(ui("当前生效：") + agent.effectiveModel, style = MaterialTheme.typography.bodySmall)
                    Text(
                        if (agent.modelSource == "agent_override") ui("Agent 自定义")
                        else ui("Auto · 运行时默认"),
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                IconButton(onClick = onRefresh, enabled = !state.modelBusy && state.online) {
                    Icon(Icons.Default.Refresh, contentDescription = ui("刷新模型"))
                }
            }
            state.modelError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            if (state.modelBusy) LinearProgressIndicator(Modifier.fillMaxWidth())
            OutlinedButton(
                onClick = { onAuto(); pendingModel = null },
                enabled = !state.modelBusy && state.online,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(ui("Auto · 恢复运行时默认"))
            }
            OutlinedTextField(
                value = search,
                onValueChange = { search = it },
                label = { Text(ui("搜索模型")) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            if (options.isEmpty() && !state.modelBusy) {
                Text(ui("暂无模型目录，请刷新后重试"), style = MaterialTheme.typography.bodySmall)
            }
            LazyColumn(
                Modifier.heightIn(max = 380.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                items(
                    options.filter { it.model.contains(search, true) || it.displayName.contains(search, true) },
                    key = { it.model },
                ) { option ->
                    OutlinedButton(
                        onClick = { pendingModel = option.model; effort = null },
                        enabled = option.available && state.online && !state.modelBusy,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.Start) {
                            Text(option.displayName)
                            Text(option.model, style = MaterialTheme.typography.bodySmall)
                            if (!option.available) {
                                Text(option.unavailableReason ?: ui("当前不可用"), style = MaterialTheme.typography.bodySmall)
                            }
                        }
                    }
                }
            }
            pendingModel?.let { model ->
                val option = options.firstOrNull { it.model == model }
                Text(ui("待应用：") + model)
                if (option?.supportsReasoningEffort == true) {
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        (listOf<String?>(null) + option.reasoningEffortOptions).forEach { value ->
                            FilterChip(
                                selected = effort == value,
                                onClick = { effort = value },
                                label = { Text(value ?: ui("默认")) },
                            )
                        }
                    }
                }
                Button(
                    onClick = { onSelect(model, effort); pendingModel = null },
                    enabled = !state.modelBusy && state.online,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(ui("应用到此 Agent"))
                }
            }
            Text(
                ui("模型更改保存到此 Agent；运行中的任务不会被切换。"),
                style = MaterialTheme.typography.bodySmall,
            )
            Spacer(Modifier.height(8.dp))
        }
    }
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
    val dragging by listState.interactionSource.collectIsDraggedAsState()
    val scope = rememberCoroutineScope()
    val recentIds = snapshot?.turns.orEmpty().mapTo(mutableSetOf(), HolonConversationTurn::id)
    val turns = state.olderTurns.filterNot { it.id in recentIds } + snapshot?.turns.orEmpty()
    val rows = conversationRows(turns)
    val latestBriefId = state.selectedAgent?.latestBrief?.briefId
    val latestBriefEventSeq = state.selectedAgent?.latestBrief?.createdEventSeq
    val agentId = state.selectedAgent?.id
    val tail = "conversation-tail"
    val itemKeys = buildList {
        if (state.hasOlderTurns) add("load-older-turns")
        if (snapshot?.pendingInputs?.isNotEmpty() == true) add("pending-inputs")
        addAll(rows.map(ConversationRow::key))
        addAll(state.outbox.map { "outbox:${it.requestId}" })
        if (turns.isEmpty() && state.outbox.isEmpty()) add("empty")
        add(tail)
    }
    val contentRevision = listOf(snapshot?.snapshotCursor, state.briefs, state.briefLoads, state.conversationDetail, state.outbox)
    LaunchedEffect(dragging) { if (dragging) position.followLatest = false }
    LaunchedEffect(listState) {
        snapshotFlow { listState.isScrollInProgress to listState.canScrollForward }.collect { (scrolling, canScroll) ->
            if (position.pendingAnchor == null && position.positionedAtLatest && listState.layoutInfo.visibleItemsInfo.isNotEmpty() && !scrolling && !canScroll) position.followLatest = true
        }
    }
    LaunchedEffect(contentRevision) {
        if (snapshot != null) {
            val justSent = state.outbox.size > position.previousOutboxCount
            val anchor = position.pendingAnchor
            if (anchor != null) {
                val anchorBrief = rows.filterIsInstance<ConversationRow.Brief>().firstOrNull { it.key == anchor }
                if (anchorBrief != null && anchorBrief.id !in state.briefs && state.briefLoads[anchorBrief.id] !is BriefLoadState.Failed) {
                    viewModel.ensureBriefs(listOf(anchorBrief.id))
                    return@LaunchedEffect
                }
                snapshotFlow { listState.layoutInfo.totalItemsCount }.first { it == itemKeys.size }
                listState.scrollToItem(readingAnchorIndex(itemKeys, anchor, listState.firstVisibleItemIndex), position.pendingOffset)
                position.pendingAnchor = null
            } else if (!position.positionedAtLatest || position.followLatest || justSent) {
                snapshotFlow { listState.layoutInfo.totalItemsCount }.first { it == itemKeys.size }
                val last = itemKeys.lastIndex
                if (last >= 0) listState.scrollToItem(last)
                position.followLatest = true
            }
            position.positionedAtLatest = true
            position.previousOutboxCount = state.outbox.size
        }
    }
    // Stable brief keys keep an individual result anchored while neighboring results load.
    LaunchedEffect(rows, latestBriefId, latestBriefEventSeq, state.briefs.keys) {
        snapshotFlow { listState.layoutInfo.visibleItemsInfo.map { it.key } }.collect { visibleKeys ->
            val visible = rows.filter { it.key in visibleKeys }
            val nearby = rows.filterIsInstance<ConversationRow.Brief>().filter { brief ->
                val index = rows.indexOf(brief)
                brief.key in visibleKeys || rows.getOrNull(index - 1)?.key?.let { it in visibleKeys } == true || rows.getOrNull(index + 1)?.key?.let { it in visibleKeys } == true
            }
            viewModel.ensureBriefs(nearby.map { it.id })
            if (agentId != null && latestBriefEventSeq != null && latestBriefId in state.briefs &&
                visible.filterIsInstance<ConversationRow.Brief>().any { it.id == latestBriefId }) {
                viewModel.markBriefRead(agentId, latestBriefEventSeq)
            }
        }
    }
    if (state.busy && snapshot == null) {
        Box(modifier.fillMaxWidth(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    Box(modifier.fillMaxWidth()) {
        LazyColumn(
            state = listState,
            modifier = Modifier.fillMaxSize(),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (state.hasOlderTurns) item(key = "load-older-turns") {
                TextButton(onClick = viewModel::loadOlderTurns, enabled = !state.historyBusy && state.historyBeforeCursor != null) {
                    if (state.historyBusy) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                    else Text(if (state.historyBeforeCursor == null) ui("更早记录暂不可读取") else ui("加载更早记录"))
                }
            }
            snapshot?.pendingInputs?.takeIf { it.isNotEmpty() }?.let { pending ->
                item(key = "pending-inputs") {
                    Text(ui("${pending.size} 条输入正在排队"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            items(rows, key = ConversationRow::key) { row ->
                when (row) {
                    is ConversationRow.Day -> Text(row.date, modifier = Modifier.fillMaxWidth().padding(top = 8.dp), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    is ConversationRow.Input -> OperatorInput(row.turn)
                    is ConversationRow.Process -> InlineTurnProcess(row.turn, state, viewModel) { position.followLatest = false }
                    is ConversationRow.Brief -> {
                        val brief = state.briefs[row.id]
                        if (brief != null) {
                            BriefContent(brief, onFile = viewModel::prepareArtifact, onWork = viewModel::openRelatedWorkItem, onDetails = { viewModel.openBrief(brief.id) })
                        } else {
                            BriefPlaceholder(
                                load = state.briefLoads[row.id],
                                offline = !state.online,
                                onRetry = { viewModel.ensureBriefs(listOf(row.id), retry = true) },
                            )
                        }
                    }
                }
            }
            items(state.outbox, key = { "outbox:${it.requestId}" }) { message ->
                LocalMessageCard(message, retryEnabled = !state.enqueueing,
                    onRetry = { viewModel.retryMessage(message) },
                    onEdit = { viewModel.editFailedMessage(message) },
                    onRemove = { viewModel.removeFailedMessage(message) })
            }
            if (turns.isEmpty() && state.outbox.isEmpty()) item(key = "empty") {
                EmptyPage(ui("开始会话"), ui("向 ${state.selectedAgent?.displayName} 说明你希望完成的工作。"))
            }
            item(key = tail) { Spacer(Modifier.height(4.dp)) }
        }
        if (!position.followLatest && listState.canScrollForward) {
            Surface(
                modifier = Modifier.align(Alignment.BottomCenter).padding(8.dp).clickable {
                    position.followLatest = true
                    scope.launch { listState.animateScrollToItem(listState.layoutInfo.totalItemsCount - 1) }
                },
                shape = RoundedCornerShape(24.dp),
                color = MaterialTheme.colorScheme.secondaryContainer,
                shadowElevation = 2.dp,
            ) { Text(ui("回到最新"), modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp), style = MaterialTheme.typography.labelLarge) }
        }
    }
}

@Composable
private fun OperatorInput(turn: HolonConversationTurn) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        turn.inputs.filter { it.presentationClass == "operator" || (it.presentationClass == null && turn.presentationClass == "operator") }.forEach { input ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(16.dp, 16.dp, 4.dp, 16.dp), modifier = Modifier.fillMaxWidth(0.92f)) {
                    Column(Modifier.padding(horizontal = 13.dp, vertical = 10.dp)) {
                        MarkdownText(input.preview.ifBlank { ui("已提交输入") })
                        input.createdAt?.let { Text(relativeTime(it), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                        input.actorDisplayName?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                    }
                }
            }
        }
    }
}

@Composable
internal fun BriefPlaceholder(load: BriefLoadState?, offline: Boolean, onRetry: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (load == BriefLoadState.Loading) CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.5.dp)
            Text(ui(if (load is BriefLoadState.Failed) { if (offline) "离线，结果未缓存" else "结果加载失败" } else if (load == BriefLoadState.Loading) "正在加载结果…" else "结果尚未加载"),
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (load is BriefLoadState.Failed) TextButton(onClick = onRetry) { Text(ui("重试")) }
        }
        if (load is BriefLoadState.Failed) Text(load.message, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
internal fun BriefContent(brief: run.holon.android.sdk.HolonBrief, onFile: (String, String) -> Unit, onWork: (String) -> Unit, onDetails: () -> Unit = {}) {
    val context = LocalContext.current
    val clipboard = LocalClipboardManager.current
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        MarkdownText(brief.text.ifBlank { ui("结果没有文本说明") })
        brief.attachments.forEach { attachment ->
            Surface(shape = RoundedCornerShape(10.dp), border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                modifier = Modifier.fillMaxWidth().clickable {
                    val uri = attachment.uri
                    if (uri?.startsWith("workspace://") == true) onFile(uri, attachment.name) else onDetails()
                }) {
                Row(Modifier.padding(horizontal = 12.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    Icon(if (attachment.kind == "image") Icons.Default.Image else Icons.Default.Description, contentDescription = null, modifier = Modifier.size(20.dp))
                    Column(Modifier.weight(1f)) {
                        Text(attachment.name, style = MaterialTheme.typography.bodyMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        if (attachment.uri?.startsWith("workspace://") != true) Text(ui("此产物暂不支持读取"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    Icon(Icons.Default.ExpandMore, contentDescription = ui("打开文件"), modifier = Modifier.size(18.dp))
                }
            }
        }
        brief.workItemId?.let { id -> ResultLinkRow(ui("关联工作"), ui("查看")) { onWork(id) } }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(localTimestamp(brief.createdAt), modifier = Modifier.weight(1f), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            IconButton(onClick = { clipboard.setText(AnnotatedString(brief.text)) }) {
                Icon(Icons.Default.ContentCopy, contentDescription = ui("复制结果"), modifier = Modifier.size(18.dp))
            }
            IconButton(onClick = { shareBrief(context, brief.text) }) {
                Icon(Icons.Default.Share, contentDescription = ui("分享结果"), modifier = Modifier.size(18.dp))
            }
        }
    }
}

@Composable
private fun InlineTurnProcess(turn: HolonConversationTurn, state: HolonUiState, viewModel: HolonViewModel, onInteraction: () -> Unit) {
    val expanded = state.selectedTurn?.id == turn.id
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).clickable { onInteraction(); if (expanded) viewModel.closeTurn() else viewModel.openTurn(turn) }.padding(vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore, contentDescription = null, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(6.dp))
            Text(ui("本轮过程"), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.weight(1f))
            val status = turn.exceptionStatus()?.first ?: if (turn.isRunning()) ui("执行中") else if (turn.briefIds.isEmpty()) turn.compactStatusText() else null
            status?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
        if (expanded) {
            val detail = state.conversationDetail
            if (detail == null && state.detailBusy) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
            if (detail == null && !state.detailBusy) TextButton(onClick = { viewModel.openTurn(turn) }) { Text(ui("重试")) }
            detail?.let {
                if (it.coverageKind != "complete") Text(detailCoverageMessage(it.coverageKind, it.coverageReason), style = MaterialTheme.typography.bodySmall)
                val activities = it.activities.filter { activity -> activity.kind != "operator" && !(activity.kind == "assistant" && activity.summary.isBlank()) }
                if (it.hasMore || activities.size > 6) Text(ui("显示最近过程，更多内容可全屏查看"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                activities.takeLast(6).forEach { activity ->
                    val open = state.selectedActivity?.id == activity.id
                    ActivityRow(activity, open, state.selectedToolExecution.takeIf { open }, open && state.detailBusy) {
                        onInteraction()
                        if (open) viewModel.closeActivity() else viewModel.inspectActivity(activity)
                    }
                }
            }
            TextButton(onClick = { viewModel.setTurnFullScreen(true) }) { Text(ui("全屏查看过程")) }
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
        isRunning() -> ui("执行中")
        terminalOutcome == "provider_failed_needs_recovery" ||
            resultKind.contains("failure", true) -> ui("本轮失败")
        terminalOutcome in setOf("aborted", "interrupted", "baseline_over_budget") -> ui("本轮未完成")
        briefIds.isNotEmpty() || resultKind == "available" -> ui("结果载入中")
        resultKind == "unavailable" -> ui("结果暂不可用")
        resultKind == "none" -> ui("没有结果摘要")
        else -> null
    }

private fun HolonConversationTurn.exceptionStatus(): Pair<String, StatusTone>? =
    when {
        attentionKind != null -> ui("需注意") to StatusTone.Warning
        resultKind.contains("failure", true) || terminalOutcome == "failure" -> ui("失败") to StatusTone.Danger
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
                    IconButton(onClick = { viewModel.setTurnFullScreen(false) }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = ui("返回结果"))
                    }
                    Column(Modifier.weight(1f)) {
                        Text(ui("本轮过程"), style = MaterialTheme.typography.headlineSmall)
                        Text(
                            if (turn.isRunning()) ui("实时更新") else ui("执行记录"),
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
                                if (input.presentationClass == "operator") ui("你的要求") else ui("触发输入"),
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Text(
                                input.preview.ifBlank { ui("已提交输入") },
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
                        else Text(if (detail.nextBeforeCursor == null) ui("更早过程暂不可读取") else ui("加载更早过程"))
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
                Text(ui("$unseenActivities 条新活动"), modifier = Modifier.padding(horizontal = 14.dp, vertical = 9.dp))
            }
        }
    }
}

private fun detailCoverageMessage(kind: String, reason: String?): String {
    val detail =
        when (reason) {
            "retention_gap" -> ui("较早的执行活动已超出保留窗口")
            "legacy_ownership" -> ui("旧版会话无法完整关联到本轮")
            "unknown_activity_type" -> ui("部分执行活动暂不支持展示")
            "missing_canonical_linkage" -> ui("部分执行活动缺少本轮关联")
            else -> if (kind == "unavailable") ui("本轮执行过程不可用") else ui("本轮仅保留了部分执行过程")
        }
    return if (kind == "unavailable") detail else ui("过程可能不完整 · $detail")
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
    if (isTool && !expanded) {
        Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).clickable(onClick = onOpen).padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Icon(Icons.Default.ExpandMore, contentDescription = ui("工具调用"), modifier = Modifier.size(18.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(activity.summary.ifBlank { ui("打开查看工具输入与输出") }, modifier = Modifier.weight(1f), style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        return
    }
    var showRaw by remember(activity.id) { mutableStateOf(false) }
    val payloadBlocks = if (detail != null) remember(detail) { activityPayloadBlocks(detail) } else emptyList()
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
                    if (isTool) ui("工具调用") else "Assistant",
                    modifier = Modifier.weight(1f),
                    style = MaterialTheme.typography.labelMedium,
                    color = if (isTool) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (isTool) {
                    Text(
                        if (expanded) ui("收起  ⌃") else ui("展开  ›"),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
            }
            if (isTool) {
                Text(
                    activity.summary.ifBlank { ui("打开查看工具输入与输出") },
                    modifier = Modifier.fillMaxWidth().clickable(onClick = onOpen).padding(vertical = 6.dp),
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            } else {
                MarkdownText(assistantActivityText(activity.summary).ifBlank { ui("（空消息）") })
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
                            payloadBlocks.forEach { block ->
                                ToolPayloadPreview(block.title, block.text, code = block.code)
                            }
                            if (detail.artifactCount > 0) {
                                Text(
                                    ui("产生 ${detail.artifactCount} 个产物"),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            TextButton(onClick = { showRaw = !showRaw }) {
                                Text(if (showRaw) ui("收起原始记录") else ui("查看原始记录"))
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
                            ui("没有可读取的工具输入或输出。"),
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
    val payloadBlocks = if (tool != null) remember(tool) { activityPayloadBlocks(tool) } else emptyList()
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = viewModel::closeActivity) { Text(ui("‹ 本轮")) }
                Column(Modifier.weight(1f)) {
                    Text(if (activity.kind == "tool") ui("工具调用") else ui("Assistant 文本"), style = MaterialTheme.typography.headlineSmall)
                    Text(activity.id, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        item {
            HolonSection(ui("摘要"), eyebrow = activity.kind.uppercase()) {
                Text(activity.summary.ifBlank { ui("没有摘要") })
            }
        }
        if (state.detailBusy && tool == null) {
            item { Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
        }
        tool?.let { detail ->
            item {
                HolonSection(detail.toolName) {
                    detail.summary?.takeIf { it != activity.summary }?.let { Text(it) }
                    if (detail.artifactCount > 0) SettingsValue(ui("产物"), detail.artifactCount.toString())
                }
            }
            payloadBlocks.forEach { block ->
                item { ToolPayloadPreview(block.title, block.text, code = block.code, limit = 4_000) }
            }
            item { TextButton(onClick = { showRaw = !showRaw }) { Text(if (showRaw) ui("收起原始记录") else ui("查看原始记录")) } }
            if (showRaw) item { ToolPayloadPreview(ui("原始记录"), detail.raw.toString(), limit = 16_000) }
        }
    }
}

@Composable
private fun ToolPayloadPreview(title: String, payload: String, code: Boolean = true, limit: Int = 2_000) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(title, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(8.dp)) {
            SelectionContainer {
                Text(
                    payload.take(limit),
                    modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(10.dp),
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = if (code) FontFamily.Monospace else FontFamily.Default,
                )
            }
        }
        if (payload.length > limit) Text(ui("预览已截断"), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
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
                Text(
                    ui("已加载 ${state.workItems.size} 项 · ${state.workItems.count { it.state !in setOf("completed", "aborted", "failed") }} 项进行中 · ${state.workItems.count { it.state == "completed" }} 项已完成"),
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
                listOf("all" to ui("全部"), "open" to ui("进行中"), "completed" to ui("已完成")).forEach { (value, label) ->
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
                                if (item.workItemId == currentId) ui("当前工作") else null,
                                item.updatedAt?.let(::relativeTime),
                                item.todoList.takeIf { it.isNotEmpty() }?.let { ui("$done/${it.size} 步") },
                            ).joinToString(" · ").ifBlank { ui("等待更多信息") },
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
                if (state.workItems.isEmpty()) ui("还没有工作记录") else ui("当前筛选没有工作记录"),
                if (state.workItems.isEmpty()) ui("Agent 的工作计划和验收结果会显示在这里。") else ui("选择“全部”查看其他工作。"),
            )
        }
        if (state.workItemsHasMore) item {
            TextButton(onClick = viewModel::loadMoreWorkItems, enabled = !state.workItemsLoadingMore, modifier = Modifier.fillMaxWidth()) {
                if (state.workItemsLoadingMore) CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                else Text(ui("加载更多工作记录"))
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
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = ui("返回工作列表"))
                    }
                }
                Column(Modifier.weight(1f)) {
                    Text(ui("工作详情"), style = MaterialTheme.typography.headlineSmall)
                    if (!finished) item.focus?.takeUnless { it.equals(item.state, ignoreCase = true) }?.let {
                        Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
                CompactStatus(workItemStatusLabel(item), workItemTone(item))
            }
        }
        item {
            HolonSection(ui("目标")) {
                Text(item.objective ?: ui("没有目标说明"), style = MaterialTheme.typography.bodyLarge)
            }
        }
        item.blockedBy?.let { blocker ->
            item { HolonSection(ui("需要处理"), eyebrow = ui("BLOCKED")) { Text(blocker, color = MaterialTheme.colorScheme.error) } }
        }
        if (finished) {
            item {
                HolonSection(ui("结果")) {
                    item.resultSummary?.takeIf(String::isNotBlank)?.let { MarkdownText(it) }
                        ?: EmptyHint(ui("这项工作没有独立的结果摘要。"))
                    item.resultBriefId?.let { briefId ->
                        ResultLinkRow(ui("查看关联 brief"), ui("查看")) { viewModel.openBrief(briefId) }
                    }
                    ResultLinkRow(ui("查看 Agent 结果"), ui("打开")) { viewModel.selectAgentSection(AgentSection.Results) }
                }
            }
        }
        if (!finished && (item.focus != null || item.schedulingState != null || item.recheckAt != null)) {
            item {
                HolonSection(ui("当前步骤")) {
                    Text(item.focus ?: ui("等待下一次调度"))
                    item.recheckAt?.let { SettingsValue(ui("再次检查"), it) }
                }
            }
        }
        if (item.todoList.isNotEmpty()) {
            item {
                if (finished) {
                    TextButton(onClick = { showSteps = !showSteps }, modifier = Modifier.fillMaxWidth()) {
                        Text(if (showSteps) ui("收起步骤记录") else ui("查看步骤记录 · $completed/${item.todoList.size}"))
                    }
                }
                if (showSteps) {
                    HolonSection(if (finished) ui("步骤记录") else ui("进度"), eyebrow = "$completed/${item.todoList.size}") {
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
                HolonSection(ui("结果")) {
                    MarkdownText(result)
                    item.resultBriefId?.let { briefId ->
                        ResultLinkRow(ui("查看关联 brief"), ui("查看")) { viewModel.openBrief(briefId) }
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
                        Text(if (state.workItemsBusy) ui("正在读取计划…") else ui("打开完整计划"))
                    }
                    TextButton(onClick = { showPlan = !showPlan }, modifier = Modifier.fillMaxWidth()) {
                        Text(if (showPlan) ui("收起计划预览") else ui("查看计划预览"))
                    }
                }
                if (showPlan) HolonSection(if (finished) ui("计划预览") else ui("计划")) {
                    if (!finished) {
                        TextButton(
                            onClick = viewModel::openWorkItemPlan,
                            enabled = !state.workItemsBusy && !plan.workspaceId.isNullOrBlank() && !plan.relativePath.isNullOrBlank(),
                        ) {
                            Text(if (state.workItemsBusy) ui("正在读取计划…") else ui("打开完整计划"))
                        }
                    }
                    if (plan.workspaceId.isNullOrBlank() || plan.relativePath.isNullOrBlank()) {
                        Text(ui("此 Holon 版本未提供计划文件的读取位置"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    MarkdownText(plan.preview ?: ui("计划文件可用，但没有内联预览。"))
                    if (!plan.previewComplete) Text(ui("此处只显示计划开头"), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        if (item.workRefs.isNotEmpty()) {
            item {
                HolonSection(ui("相关工作"), eyebrow = item.workRefs.size.toString()) {
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
                Text(if (showDetails) ui("收起技术详情") else ui("技术详情"))
            }
        }
        if (showDetails) {
            item {
                HolonSection(ui("技术详情")) {
                    SettingsValue("ID", item.workItemId)
                    SettingsValue(ui("状态"), item.state)
                    item.updatedAt?.let { SettingsValue(ui("更新"), it) }
                    item.revision?.let { SettingsValue(ui("版本"), it.toString()) }
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
                title = ui("文件"),
                onBack = viewModel::returnFromMessageFile,
                backLabel = if (state.fileLinkOrigin != null || state.agentSection == AgentSection.Results || state.selectedBrief != null) ui("消息") else ui("文件列表"),
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
                Text(if (state.fileLinkOrigin != null || state.agentSection == AgentSection.Results || state.selectedBrief != null) ui("‹ 返回消息") else ui("‹ 返回文件列表"))
            }
            Text(prepared.fileName, style = MaterialTheme.typography.headlineSmall)
            Text(prepared.mediaType, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            ArtifactPreview(prepared)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = { saveFile.launch(prepared.fileName) }) { Text(ui("保存到设备")) }
                TextButton(onClick = { shareArtifact(context, prepared) }) { Text(ui("分享或打开")) }
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
                    ui("最近产物"),
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    style = MaterialTheme.typography.titleMedium,
                )
            }
            items(recentArtifacts, key = { it.uri.orEmpty() }) { attachment ->
                Box(Modifier.padding(horizontal = 16.dp)) {
                    ResultLinkRow(
                        label = attachment.name,
                        meta = ui("预览"),
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
                                    ui("${if (workspace.isActive) "当前 · " else ""}${workspace.label}"),
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
                    placeholder = { Text(ui("查找当前文件夹")) },
                    singleLine = true,
                    modifier = Modifier.weight(1f),
                )
                FilterChip(
                    selected = position.showHidden,
                    onClick = { position.showHidden = !position.showHidden },
                    label = { Text(ui("隐藏文件")) },
                )
                FilterChip(
                    selected = position.sortRecent,
                    onClick = { position.sortRecent = !position.sortRecent },
                    label = { Text(if (position.sortRecent) ui("最新") else ui("名称")) },
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
        if (!state.workspaceBusy && state.workspaces.isEmpty()) item { EmptyPage(ui("没有可浏览的 workspace"), ui("Agent 尚未连接可访问的工作区。")) }
        if (!state.workspaceBusy && state.workspaces.isNotEmpty() && visibleEntries.isEmpty()) {
            item {
                EmptyPage(
                    if (state.workspaceDirectory?.entries.isNullOrEmpty()) ui("这个文件夹是空的") else ui("没有匹配的文件"),
                    if (state.workspaceDirectory?.entries.isNullOrEmpty()) ui("返回上一级继续浏览。") else ui("调整搜索或显示隐藏文件。"),
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
            "pending" -> ui("待发送")
            "sending" -> ui("发送中")
            "received" -> ui("已接收")
            "unknown" -> ui("结果未知 · 将安全重试")
            "failed" -> ui("发送失败")
            else -> message.state
        }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        Surface(
            color = MaterialTheme.colorScheme.primaryContainer,
            shape = RoundedCornerShape(14.dp, 14.dp, 3.dp, 14.dp),
            modifier = Modifier.fillMaxWidth(0.88f),
        ) {
            Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                Text(message.text.ifBlank { ui("附件") })
                Text(
                    label,
                    style = MaterialTheme.typography.labelSmall,
                    color = if (message.state == "failed") MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onPrimaryContainer,
                )
                message.error?.let { Text(ui(it), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
                if (message.state in setOf("failed", "unknown")) {
                    Row {
                        TextButton(onClick = onRetry, enabled = retryEnabled) { Text(ui("重试")) }
                        if (message.state == "failed") {
                            TextButton(onClick = onEdit, enabled = retryEnabled) { Text(ui("编辑")) }
                            TextButton(onClick = onRemove, enabled = retryEnabled) { Text(ui("本机移除")) }
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
    MessageComposer(
        draft = state.draft,
        attachments = state.attachments,
        sending = state.enqueueing,
        staging = state.stagingAttachment,
        canStop = state.selectedAgent?.currentRunId != null,
        stopping = state.abortingRun,
        onDraft = viewModel::updateDraft,
        onSend = viewModel::send,
        onStop = viewModel::stopCurrentTurn,
        onRemove = viewModel::removeAttachment,
        onImage = onImage,
        onFile = onFile,
        onCamera = onCamera,
    )
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
                TextButton(onClick = viewModel::closeBrief) { Text(ui("‹ 会话")) }
                Column(Modifier.weight(1f)) {
                    Text(ui("产物与关联工作"), style = MaterialTheme.typography.headlineSmall)
                    Text(brief.createdAt, style = MaterialTheme.typography.labelSmall)
                }
                IconButton(onClick = { shareBrief(context, brief.text) }) {
                    Icon(Icons.Default.Share, contentDescription = ui("分享结果"))
                }
            }
        }
        brief.workItemId?.let { id ->
            item {
                HolonSection(ui("关联工作"), eyebrow = ui("WORK ITEM")) {
                    val item = state.workItems.firstOrNull { it.workItemId == id }
                    Text(item?.objective ?: ui("这项工作的详情可直接打开，不依赖工作列表是否已加载。"))
                    ResultLinkRow(ui("查看工作详情"), if (state.busy) ui("正在读取") else ui("打开")) {
                        viewModel.openRelatedWorkItem(id)
                    }
                }
            }
        }
        if (brief.attachments.isNotEmpty()) {
            item {
                Text(ui("产物"), style = MaterialTheme.typography.headlineSmall)
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
                                attachment.uri == null -> ui("此产物没有可读取 locator")
                                attachment.uri?.startsWith("workspace://") == true -> ui("受保护的工作区产物，可通过当前 session 读取")
                                else -> ui("不支持的产物 locator")
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
                                Text(if (state.busy) ui("正在读取…") else ui("预览产物"))
                            }
                            if (prepared?.locator == locator) {
                                ArtifactPreview(prepared)
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    TextButton(onClick = { saveArtifact.launch(prepared.fileName) }) { Text(ui("下载")) }
                                    TextButton(onClick = { shareArtifact(context, prepared) }) { Text(ui("分享")) }
                                    TextButton(onClick = viewModel::clearPreparedArtifact) { Text(ui("关闭预览")) }
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
    context.startActivity(Intent.createChooser(intent, ui("分享结果")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
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
                    contentDescription = ui("${artifact.fileName}，点按放大"),
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
                            ) { Icon(Icons.Default.Close, contentDescription = ui("关闭图片预览"), tint = Color.White) }
                        }
                    }
                }
            } else {
                EmptyHint(if (bitmap == null) ui("正在读取图片…") else ui("图片无法预览，可保存或分享后打开"))
            }
        }
        else -> EmptyHint(ui("${artifact.mediaType} 不支持内置预览，可下载或分享后打开"))
    }
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
        seconds < 60 -> ui("刚刚")
        seconds < 3_600 -> ui("${seconds / 60} 分钟前")
        seconds < 86_400 -> ui("${seconds / 3_600} 小时前")
        seconds < 604_800 -> ui("${seconds / 86_400} 天前")
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
        seconds < 60 -> ui("刚刚")
        seconds < 3_600 -> ui("${seconds / 60} 分钟前")
        seconds < 86_400 -> ui("${seconds / 3_600} 小时前")
        seconds < 604_800 -> ui("${seconds / 86_400} 天前")
        else -> ui("${seconds / 604_800} 周前")
    }
}

private fun workItemStatusLabel(item: HolonWorkItemSnapshot): String =
    when {
        item.blockedBy != null -> ui("受阻")
        item.state == "completed" -> ui("已完成")
        item.readiness == "ready" -> ui("可继续")
        item.schedulingState == "waiting" -> ui("等待中")
        else -> item.readiness ?: item.state
    }

private fun HolonWorkItemSnapshot.listSummary(): String =
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
    context.startActivity(Intent.createChooser(intent, ui("分享 ${artifact.fileName}")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
}

private fun formatBytes(value: Long): String =
    when {
        value >= 1024 * 1024 -> "%.1f MB".format(value / 1024.0 / 1024.0)
        value >= 1024 -> "%.1f KB".format(value / 1024.0)
        else -> "$value B"
    }
