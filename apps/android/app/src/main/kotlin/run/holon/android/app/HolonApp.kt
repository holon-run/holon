package run.holon.android.app

import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.Image
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import java.io.File
import java.util.UUID
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonConversationTurn

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
    Box(
        Modifier.fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(horizontal = 24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier.fillMaxWidth(),
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
                label = { Text("token") },
                supportingText = { Text("仅用于换取可撤销 session，不会保存在设备上") },
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
    val navController = rememberNavController()
    val entry by navController.currentBackStackEntryAsState()
    val route = entry?.destination?.route
    val showBottomBar = route != "conversation"

    LaunchedEffect(state.selectedAgent?.id) {
        if (state.selectedAgent != null && route != "conversation") navController.navigate("conversation")
    }

    Scaffold(
        modifier = Modifier.fillMaxSize(),
        containerColor = MaterialTheme.colorScheme.background,
        bottomBar = {
            if (showBottomBar) {
                NavigationBar(containerColor = MaterialTheme.colorScheme.surface) {
                    MainDestination.entries.forEach { destination ->
                        NavigationBarItem(
                            selected = route == destination.route,
                            onClick = {
                                navController.navigate(destination.route) {
                                    popUpTo(MainDestination.Recent.route) { saveState = true }
                                    launchSingleTop = true
                                    restoreState = true
                                }
                            },
                            icon = { Text(if (route == destination.route) "●" else "○") },
                            label = { Text(destination.label) },
                        )
                    }
                }
            }
        },
    ) { padding ->
        NavHost(
            navController = navController,
            startDestination = MainDestination.Recent.route,
            modifier = Modifier.padding(padding),
        ) {
            composable(MainDestination.Recent.route) { RecentScreen(state, viewModel) }
            composable(MainDestination.Agents.route) { AgentsScreen(state, viewModel) }
            composable(MainDestination.Settings.route) { SettingsScreen(state, viewModel) }
            composable("conversation") {
                ConversationScreen(
                    state = state,
                    viewModel = viewModel,
                    onBack = {
                        viewModel.closeConversation()
                        navController.popBackStack()
                    },
                )
            }
        }
    }
}

@Composable
private fun PageHeader(title: String, subtitle: String, state: HolonUiState, onRefresh: (() -> Unit)? = null) {
    Column(
        modifier = Modifier.fillMaxWidth().statusBarsPadding().padding(horizontal = 20.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(7.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.headlineLarge)
                Text(subtitle, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
            }
            StatusPill(if (state.online) "在线" else "离线", if (state.online) StatusTone.Success else StatusTone.Warning)
            onRefresh?.let {
                Spacer(Modifier.width(6.dp))
                TextButton(onClick = it, enabled = !state.busy) { Text("刷新") }
            }
        }
        state.statusMessage?.let { Text(it, color = MaterialTheme.colorScheme.tertiary, style = MaterialTheme.typography.bodySmall) }
        state.error?.let { ErrorBanner(it, null) }
    }
}

@Composable
private fun RecentScreen(state: HolonUiState, viewModel: HolonViewModel) {
    Column(Modifier.fillMaxSize()) {
        PageHeader("最近", "需要你回应的会话会优先出现", state) { viewModel.refresh() }
        val recent = state.recentAgents.filter {
            it.needsReply() || it.latestBrief != null || it.schedulingPosture !in setOf("idle", "stopped", "unknown")
        }.ifEmpty { state.recentAgents }
        if (recent.isEmpty()) {
            EmptyPage("还没有会话", "Agent 有新活动后会出现在这里。你也可以从 Agents 开始。")
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 16.dp, vertical = 4.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                items(recent, key = AgentSummary::id) { agent ->
                    AgentConversationRow(agent, onClick = { viewModel.openAgent(agent) })
                }
                item { Spacer(Modifier.height(12.dp)) }
            }
        }
    }
}

@Composable
private fun AgentsScreen(state: HolonUiState, viewModel: HolonViewModel) {
    Column(Modifier.fillMaxSize()) {
        PageHeader("Agents", "${state.agents.size} 个可见 Agent", state) { viewModel.refresh() }
        OutlinedTextField(
            value = state.search,
            onValueChange = viewModel::setSearch,
            label = { Text("搜索 Agent") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        )
        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(9.dp),
        ) {
            items(state.filteredAgents, key = AgentSummary::id) { agent ->
                AgentConversationRow(agent, compact = true, onClick = { viewModel.openAgent(agent) })
            }
        }
    }
}

@Composable
private fun AgentConversationRow(agent: AgentSummary, compact: Boolean = false, onClick: () -> Unit) {
    val tone = agent.statusTone()
    val briefPreview = agent.latestBrief?.preview.orEmpty()
    val postureReason = agent.postureReason.orEmpty()
    Surface(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
        color = MaterialTheme.colorScheme.surface,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Row {
            Box(
                Modifier.width(4.dp).height(if (compact) 86.dp else 108.dp)
                    .background(toneColor(tone), RoundedCornerShape(topStart = 12.dp, bottomStart = 12.dp)),
            )
            Column(
                modifier = Modifier.weight(1f).padding(horizontal = 14.dp, vertical = 13.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(agent.displayName, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                    StatusPill(agent.statusLabel(), tone)
                }
                Text(
                    when {
                        agent.needsReply() -> "正在等你回应"
                        briefPreview.isNotBlank() -> briefPreview
                        postureReason.isNotBlank() -> postureReason
                        else -> "暂无新的工作摘要"
                    },
                    maxLines = if (compact) 1 else 2,
                    overflow = TextOverflow.Ellipsis,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodyMedium,
                )
                if (!compact) {
                    Text(
                        listOfNotNull(agent.id, agent.currentWorkItemId?.let { "work $it" }).joinToString(" · "),
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
private fun SettingsScreen(state: HolonUiState, viewModel: HolonViewModel) {
    Column(Modifier.fillMaxSize()) {
        PageHeader("设置", "连接、身份与兼容信息", state) { viewModel.refresh() }
        LazyColumn(
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 16.dp, vertical = 4.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item {
                HolonSection("当前连接", eyebrow = "RUNTIME") {
                    SettingsValue("地址", state.session?.baseUrl.orEmpty())
                    SettingsValue("状态", if (state.online) "已连接" else "离线缓存")
                    SettingsValue("Runtime", state.session?.runtimeId.orEmpty())
                }
            }
            item {
                HolonSection("身份", eyebrow = "SESSION") {
                    SettingsValue("用户", state.session?.user?.displayName ?: state.session?.user?.userId.orEmpty())
                    SettingsValue("认证", state.session?.user?.authMethod.orEmpty())
                    Text("session 保存在 Android Keystore；原始 token 不会保存。", style = MaterialTheme.typography.bodySmall)
                }
            }
            item {
                HolonSection("兼容性", eyebrow = "PROTOCOL") {
                    SettingsValue("协议", "holon-control/1")
                    SettingsValue("能力", state.session?.server?.capabilities?.size?.toString().orEmpty())
                    Text(
                        state.session?.server?.capabilities?.sorted()?.joinToString("\n").orEmpty(),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            item {
                OutlinedButton(
                    onClick = viewModel::relogin,
                    enabled = !state.busy,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("重新登录当前主机") }
            }
            item {
                OutlinedButton(
                    onClick = viewModel::logout,
                    enabled = !state.busy,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("退出并清除本机数据") }
            }
            item { Spacer(Modifier.height(16.dp)) }
        }
    }
}

@Composable
private fun SettingsValue(label: String, value: String) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Text(label, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.width(72.dp))
        Text(value.ifBlank { "—" }, modifier = Modifier.weight(1f))
    }
}

@Composable
private fun ConversationScreen(state: HolonUiState, viewModel: HolonViewModel, onBack: () -> Unit) {
    val agent = state.selectedAgent ?: return
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

    Column(Modifier.fillMaxSize().statusBarsPadding()) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 7.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(onClick = onBack) { Text("‹ 返回") }
            Column(Modifier.weight(1f)) {
                Text(agent.displayName, style = MaterialTheme.typography.titleMedium)
                Text(
                    agent.currentWorkItemId?.let { "当前工作 · $it" } ?: agent.statusLabel(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            StatusPill(agent.statusLabel(), agent.statusTone())
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        state.error?.let { ErrorBanner(it, viewModel::clearError) }
        if (state.selectedBrief != null) {
            BriefScreen(state, viewModel)
        } else {
            ConversationTimeline(state, viewModel, Modifier.weight(1f))
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
    }
}

@Composable
private fun ConversationTimeline(state: HolonUiState, viewModel: HolonViewModel, modifier: Modifier) {
    val snapshot = state.conversation
    if (state.busy && snapshot == null) {
        Box(modifier.fillMaxWidth(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    LazyColumn(
        modifier = modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 14.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
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
        items(snapshot?.turns.orEmpty(), key = HolonConversationTurn::id) { turn ->
            TurnCard(turn, viewModel::openBrief)
        }
        items(state.outbox, key = OutboxEntity::requestId) { message ->
            LocalMessageCard(message)
        }
        if (snapshot?.turns.isNullOrEmpty() && state.outbox.isEmpty()) {
            item { EmptyPage("开始会话", "向 ${state.selectedAgent?.displayName} 说明你希望完成的工作。") }
        }
    }
}

@Composable
private fun TurnCard(turn: HolonConversationTurn, onBrief: (String) -> Unit) {
    var expanded by remember(turn.id) { mutableStateOf(false) }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        turn.inputs.filter { it.presentationClass != "internal" }.forEach { input ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Surface(
                    color = MaterialTheme.colorScheme.primaryContainer,
                    shape = RoundedCornerShape(14.dp, 14.dp, 3.dp, 14.dp),
                    modifier = Modifier.fillMaxWidth(0.88f),
                ) {
                    Column(Modifier.padding(12.dp)) {
                        Text(input.preview.ifBlank { "已提交输入" })
                        input.actorDisplayName?.let {
                            Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onPrimaryContainer)
                        }
                    }
                }
            }
        }
        Surface(
            color = MaterialTheme.colorScheme.surface,
            border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
            shape = RoundedCornerShape(12.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Column(Modifier.padding(13.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        when (turn.summary) {
                            "Work result available" -> "已有工作结果"
                            "Work failed" -> "工作失败"
                            "Work in progress" -> "工作进行中"
                            else -> turn.summary
                        },
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    StatusPill(turn.resultLabel(), turn.resultTone())
                }
                TextButton(onClick = { expanded = !expanded }, contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp)) {
                    Text(if (expanded) "收起活动摘要" else "查看活动摘要")
                }
                AnimatedVisibility(expanded) {
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        SettingsValue("执行", turn.executionKind)
                        SettingsValue("结果", turn.resultKind)
                        turn.terminalOutcome?.let { SettingsValue("结束", it) }
                        turn.attentionKind?.let { SettingsValue("注意", it) }
                    }
                }
                turn.briefIds.forEach { briefId ->
                    OutlinedButton(onClick = { onBrief(briefId) }, modifier = Modifier.fillMaxWidth()) {
                        Text("打开结果 brief")
                    }
                }
            }
        }
    }
}

@Composable
private fun LocalMessageCard(message: OutboxEntity) {
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
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shadowElevation = 10.dp,
        modifier = Modifier.fillMaxWidth().imePadding(),
    ) {
        Column(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp).navigationBarsPadding(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (state.attachments.isNotEmpty()) {
                Column(verticalArrangement = Arrangement.spacedBy(5.dp)) {
                    state.attachments.forEachIndexed { index, attachment ->
                        Row(
                            Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(8.dp)).padding(9.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(if (attachment.kind == "image") "图片" else "文件", style = MaterialTheme.typography.labelSmall)
                            Spacer(Modifier.width(8.dp))
                            Text(
                                "${attachment.name} · ${formatBytes(attachment.size)}",
                                modifier = Modifier.weight(1f),
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            TextButton(onClick = { viewModel.removeAttachment(index) }) { Text("移除") }
                        }
                    }
                }
            }
            OutlinedTextField(
                value = state.draft,
                onValueChange = viewModel::updateDraft,
                placeholder = { Text("输入消息…") },
                minLines = 1,
                maxLines = 4,
                modifier = Modifier.fillMaxWidth(),
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = onImage) { Text("相册") }
                TextButton(onClick = onCamera) { Text("拍照") }
                TextButton(onClick = onFile) { Text("文件") }
                Spacer(Modifier.weight(1f))
                Button(
                    onClick = viewModel::send,
                    enabled = state.draft.isNotBlank() || state.attachments.isNotEmpty(),
                    shape = RoundedCornerShape(9.dp),
                ) { Text("发送") }
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
        target?.let { uri ->
            runCatching {
                context.contentResolver.openOutputStream(uri)?.use { output ->
                    File(source.localPath).inputStream().use { it.copyTo(output) }
                } ?: error("无法写入所选位置")
            }
        }
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
                    Text("工作结果", style = MaterialTheme.typography.headlineSmall)
                    Text(brief.createdAt, style = MaterialTheme.typography.labelSmall)
                }
                StatusPill("brief", StatusTone.Success)
            }
        }
        item {
            HolonSection("结果", eyebrow = brief.kind) {
                Text(brief.text.ifBlank { "结果没有文本说明" }, style = MaterialTheme.typography.bodyLarge)
            }
        }
        brief.workItemId?.let { id ->
            item {
                HolonSection("关联工作", eyebrow = "WORK ITEM") {
                    val item = state.workItems.firstOrNull { it.workItemId == id }
                    SettingsValue("ID", id)
                    SettingsValue("状态", item?.state ?: "未找到或无权限")
                    item?.objective?.let { Text(it) }
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

@Composable
private fun ArtifactPreview(artifact: PreparedArtifact) {
    val file = remember(artifact.localPath) { File(artifact.localPath) }
    when {
        artifact.mediaType.startsWith("image/") -> {
            val bitmap = remember(artifact.localPath) { BitmapFactory.decodeFile(file.absolutePath) }
            if (bitmap != null) {
                Image(
                    bitmap = bitmap.asImageBitmap(),
                    contentDescription = artifact.fileName,
                    modifier = Modifier.fillMaxWidth().height(240.dp),
                )
            } else {
                EmptyHint("图片已不存在或无法解码")
            }
        }
        artifact.mediaType.startsWith("text/") || artifact.mediaType == "application/json" -> {
            val preview = remember(artifact.localPath) {
                runCatching { file.bufferedReader().use { it.readText().take(20_000) } }
                    .getOrElse { "无法读取文本预览" }
            }
            Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(8.dp)) {
                Text(preview, modifier = Modifier.fillMaxWidth().padding(10.dp), style = MaterialTheme.typography.bodySmall)
            }
        }
        else -> EmptyHint("${artifact.mediaType} 不支持内置预览，可下载或分享后打开")
    }
}

@Composable
private fun ErrorBanner(message: String, onDismiss: (() -> Unit)?) {
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
private fun EmptyPage(title: String, text: String) {
    Box(Modifier.fillMaxWidth().padding(28.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            Text(text, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

private fun AgentSummary.statusTone(): StatusTone =
    when {
        needsReply() -> StatusTone.Warning
        schedulingPosture == "blocked" -> StatusTone.Danger
        schedulingPosture == "active_turn" || runtimeStatus.lowercase() in setOf("running", "active") -> StatusTone.Accent
        schedulingPosture in setOf("waiting_for_external", "waiting_for_task") -> StatusTone.Neutral
        else -> StatusTone.Success
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

private fun HolonConversationTurn.resultLabel(): String =
    when {
        attentionKind != null -> "需注意"
        resultKind.contains("failure", true) || terminalOutcome == "failure" -> "失败"
        resultKind == "available" -> "结果可用"
        settled -> "已完成"
        executionKind.contains("active", true) -> "执行中"
        else -> "进行中"
    }

private fun HolonConversationTurn.resultTone(): StatusTone =
    when (resultLabel()) {
        "需注意" -> StatusTone.Warning
        "失败" -> StatusTone.Danger
        "已完成", "结果可用" -> StatusTone.Success
        else -> StatusTone.Accent
    }

@Composable
private fun toneColor(tone: StatusTone): Color =
    when (tone) {
        StatusTone.Neutral -> MaterialTheme.colorScheme.outline
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
