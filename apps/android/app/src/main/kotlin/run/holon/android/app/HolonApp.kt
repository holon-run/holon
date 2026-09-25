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
import androidx.compose.foundation.Image
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
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
import java.io.File
import java.util.UUID
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
    val showConversation = state.selectedAgent != null
    val showBottomBar = !showConversation

    Scaffold(
        modifier = Modifier.fillMaxSize(),
        containerColor = MaterialTheme.colorScheme.background,
        bottomBar = {
            if (showBottomBar) {
                NavigationBar(containerColor = MaterialTheme.colorScheme.surface) {
                    MainDestination.entries.forEach { destination ->
                        NavigationBarItem(
                            selected = state.mainDestination == destination,
                            onClick = { viewModel.selectMainDestination(destination) },
                            icon = { Text(if (state.mainDestination == destination) "●" else "○") },
                            label = { Text(destination.label) },
                        )
                    }
                }
            }
        },
    ) { padding ->
        if (showConversation) {
            ConversationScreen(
                state = state,
                viewModel = viewModel,
                onBack = viewModel::closeConversation,
            )
        } else {
            Box(Modifier.fillMaxSize().padding(padding)) {
                when (state.mainDestination) {
                    MainDestination.Agents -> AgentsScreen(state, viewModel)
                    MainDestination.Settings -> SettingsScreen(state, viewModel)
                }
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
private fun AgentsScreen(state: HolonUiState, viewModel: HolonViewModel) {
    Column(Modifier.fillMaxSize()) {
        PageHeader("Agents", "${state.agents.size} 个 Agent · 需回应和最近活动优先", state) { viewModel.refresh() }
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
                AgentConversationRow(agent, onClick = { viewModel.openAgent(agent) })
            }
            if (state.filteredAgents.isEmpty()) item { EmptyPage("没有匹配的 Agent", "换一个名称或 ID 再试。") }
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
        AgentSectionBar(state.agentSection, viewModel::selectAgentSection)
        if (state.selectedBrief != null) {
            BriefScreen(state, viewModel)
        } else if (state.selectedActivity != null) {
            ActivityDetailScreen(state, viewModel)
        } else if (state.selectedTurn != null) {
            TurnDetailScreen(state, viewModel)
        } else if (state.selectedWorkItem != null) {
            WorkItemDetailScreen(state, viewModel)
        } else {
            when (state.agentSection) {
                AgentSection.Results -> {
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
                AgentSection.Work -> WorkItemsScreen(state, viewModel, Modifier.weight(1f))
                AgentSection.Files -> WorkspaceBrowserScreen(state, viewModel, Modifier.weight(1f))
            }
        }
    }
}

@Composable
private fun AgentSectionBar(selected: AgentSection, onSelect: (AgentSection) -> Unit) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        AgentSection.entries.forEach { section ->
            val active = section == selected
            if (active) {
                Button(
                    onClick = { onSelect(section) },
                    modifier = Modifier.weight(1f),
                    shape = RoundedCornerShape(9.dp),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 8.dp),
                ) { Text(section.label) }
            } else {
                TextButton(
                    onClick = { onSelect(section) },
                    modifier = Modifier.weight(1f),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = 8.dp),
                ) { Text(section.label, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            }
        }
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
}

@Composable
private fun ConversationTimeline(state: HolonUiState, viewModel: HolonViewModel, modifier: Modifier) {
    val snapshot = state.conversation
    val listState = rememberLazyListState()
    var positionedAtLatest by remember(state.selectedAgent?.id) { mutableStateOf(false) }
    LaunchedEffect(snapshot?.turns?.size, state.outbox.size) {
        if (!positionedAtLatest && snapshot != null) {
            val pendingRows = if (snapshot.pendingInputs.isNotEmpty()) 1 else 0
            val itemCount = pendingRows + snapshot.turns.size + state.outbox.size
            if (itemCount > 0) listState.scrollToItem(itemCount - 1)
            positionedAtLatest = true
        }
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
            TurnCard(
                turn = turn,
                brief = turn.briefIds.firstNotNullOfOrNull(state.briefs::get),
                onRelatedContent = viewModel::openBrief,
                onDetail = { viewModel.openTurn(turn) },
            )
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
private fun TurnCard(
    turn: HolonConversationTurn,
    brief: run.holon.android.sdk.HolonBrief?,
    onRelatedContent: (String) -> Unit,
    onDetail: () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        turn.inputs.filter {
            it.presentationClass == "operator" ||
                (it.presentationClass == null && turn.presentationClass == "operator")
        }.forEach { input ->
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
            color = if (brief != null) MaterialTheme.colorScheme.surface else MaterialTheme.colorScheme.surfaceVariant,
            border = BorderStroke(1.dp, if (brief != null) MaterialTheme.colorScheme.primary.copy(alpha = 0.28f) else MaterialTheme.colorScheme.outlineVariant),
            shape = RoundedCornerShape(4.dp, 12.dp, 12.dp, 4.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Row(Modifier.height(IntrinsicSize.Min)) {
                Box(
                    Modifier.width(3.dp).fillMaxHeight()
                        .background(if (brief != null) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outline),
                )
                Column(Modifier.weight(1f).padding(13.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (brief != null) "工作结果" else turn.displaySummary(),
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.labelLarge,
                        color = if (brief != null) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,
                    )
                    StatusPill(turn.resultLabel(), turn.resultTone())
                }
                if (brief != null) {
                    MarkdownText(brief.text.ifBlank { "结果没有文本说明" })
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        if (brief.attachments.isNotEmpty() || brief.workItemId != null) {
                            TextButton(
                                onClick = { onRelatedContent(brief.id) },
                                contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 0.dp),
                            ) { Text("产物与关联工作") }
                        }
                        TextButton(
                            onClick = onDetail,
                            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 0.dp),
                        ) { Text("本轮过程") }
                    }
                } else {
                    Text(
                        when (turn.executionKind) {
                            "active" -> "Agent 正在处理；完成后结果会出现在这里。"
                            "terminal" -> "本轮没有可读 brief，可查看 assistant 与工具活动。"
                            else -> "正在同步本轮状态。"
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    TextButton(
                        onClick = onDetail,
                        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 0.dp),
                    ) { Text("查看本轮过程") }
                }
                }
            }
        }
    }
}

private fun HolonConversationTurn.displaySummary(): String =
    when (summary) {
        "Work result available" -> "已有工作结果"
        "Work failed" -> "工作失败"
        "Work in progress" -> "工作进行中"
        else -> summary
    }

@Composable
private fun TurnDetailScreen(state: HolonUiState, viewModel: HolonViewModel) {
    val turn = state.selectedTurn ?: return
    val listState = rememberLazyListState()
    val detail = state.conversationDetail
    val activities =
        detail?.activities.orEmpty().filter {
            it.kind != "operator" && !(it.kind == "assistant" && it.summary.isBlank())
        }
    val latestActivityRevision = activities.lastOrNull()?.let { "${it.id}:${it.revision}" }
    LaunchedEffect(turn.id, latestActivityRevision) {
        if (activities.isNotEmpty()) {
            val inputCount = turn.inputs.count { it.presentationClass != "internal" }
            val coverageCount = if (detail?.coverageKind != null && detail.coverageKind != "complete") 1 else 0
            val latestIndex = 1 + inputCount + coverageCount + activities.lastIndex
            listState.animateScrollToItem(latestIndex)
        }
    }
    LazyColumn(
        state = listState,
        modifier = Modifier.fillMaxSize(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = viewModel::closeTurn) { Text("‹ 结果") }
                Column(Modifier.weight(1f)) {
                    Text("本轮过程", style = MaterialTheme.typography.headlineSmall)
                    Text(
                        if (turn.executionKind == "active") "实时更新 · 新活动自动跟随" else turn.id,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                StatusPill(if (turn.executionKind == "active") "实时" else turn.resultLabel(), turn.resultTone())
            }
        }
        turn.inputs.filter { it.presentationClass != "internal" }.forEach { input ->
            item(key = input.messageId) {
                HolonSection(
                    if (input.presentationClass == "operator") "Operator 输入" else "触发输入",
                    eyebrow = input.presentationClass?.uppercase() ?: "INPUT",
                ) {
                    Text(
                        input.preview.ifBlank { "已提交输入" },
                        maxLines = 20,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
        if (state.detailBusy && state.conversationDetail == null) {
            item { Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
        }
        detail?.let {
            if (detail.coverageKind != "complete") {
                item {
                    Text(
                        detailCoverageMessage(detail.coverageKind, detail.coverageReason),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            items(
                activities,
                key = HolonConversationActivity::id,
            ) { activity ->
                ActivityRow(activity) { viewModel.inspectActivity(activity) }
            }
        }
        item { Spacer(Modifier.height(24.dp)) }
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
private fun ActivityRow(activity: HolonConversationActivity, onOpen: () -> Unit) {
    val isTool = activity.kind == "tool"
    Surface(
        color = if (isTool) MaterialTheme.colorScheme.surfaceVariant else MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        shape = RoundedCornerShape(10.dp),
        modifier = Modifier.fillMaxWidth().clickable(onClick = onOpen),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
            Text(
                when (activity.kind) {
                    "assistant" -> "ASSISTANT"
                    "tool" -> "工具调用"
                    else -> activity.kind.uppercase()
                },
                style = MaterialTheme.typography.labelSmall,
                color = if (isTool) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                activity.summary.ifBlank { if (isTool) "打开查看工具输入与输出" else "（空消息）" },
                style = if (isTool) MaterialTheme.typography.bodyMedium else MaterialTheme.typography.bodyLarge,
                maxLines = if (isTool) 3 else 12,
                overflow = TextOverflow.Ellipsis,
            )
            if (isTool) Text("查看详情 ›", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
        }
    }
}

@Composable
private fun ActivityDetailScreen(state: HolonUiState, viewModel: HolonViewModel) {
    val activity = state.selectedActivity ?: return
    val tool = state.selectedToolExecution
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
                HolonSection(detail.toolName, eyebrow = "${detail.status} · TOOL") {
                    detail.summary?.let { Text(it) }
                    SettingsValue("产物", detail.artifactCount.toString())
                }
            }
            item {
                HolonSection("输入与输出", eyebrow = "RAW DETAIL") {
                    Text(
                        detail.raw.toString().take(16_000),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun WorkItemsScreen(state: HolonUiState, viewModel: HolonViewModel, modifier: Modifier) {
    val currentId = state.selectedAgent?.currentWorkItemId
    val sorted = state.workItems.sortedWith(
        compareByDescending<HolonWorkItemSnapshot> { it.workItemId == currentId }
            .thenBy { it.state == "completed" }
            .thenByDescending { it.updatedAt.orEmpty() },
    )
    LazyColumn(
        modifier = modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(14.dp),
        verticalArrangement = Arrangement.spacedBy(9.dp),
    ) {
        item {
            Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text("WorkItems", style = MaterialTheme.typography.headlineSmall)
                Text("计划、进度、等待与最终结果", color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        if (state.workItemsBusy && sorted.isEmpty()) {
            item { Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
        }
        items(sorted, key = HolonWorkItemSnapshot::workItemId) { item ->
            Surface(
                color = MaterialTheme.colorScheme.surface,
                border = BorderStroke(1.dp, if (item.workItemId == currentId) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outlineVariant),
                shape = RoundedCornerShape(10.dp),
                modifier = Modifier.fillMaxWidth().clickable { viewModel.openWorkItem(item) },
            ) {
                Column(Modifier.padding(13.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            item.objective ?: item.workItemId,
                            modifier = Modifier.weight(1f),
                            maxLines = 3,
                            overflow = TextOverflow.Ellipsis,
                            fontWeight = FontWeight.SemiBold,
                        )
                        StatusPill(item.readiness ?: item.state, if (item.state == "completed") StatusTone.Success else StatusTone.Accent)
                    }
                    Text(
                        listOfNotNull(if (item.workItemId == currentId) "当前" else null, item.focus, item.schedulingState).joinToString(" · "),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(item.workItemId, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        if (!state.workItemsBusy && sorted.isEmpty()) item { EmptyPage("还没有 WorkItem", "Agent 的工作计划和验收结果会显示在这里。") }
    }
}

@Composable
private fun WorkItemDetailScreen(state: HolonUiState, viewModel: HolonViewModel) {
    val item = state.selectedWorkItem ?: return
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = viewModel::closeWorkItem) { Text("‹ 工作") }
                Column(Modifier.weight(1f)) {
                    Text("WorkItem", style = MaterialTheme.typography.headlineSmall)
                    Text(item.workItemId, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                StatusPill(item.readiness ?: item.state, if (item.state == "completed") StatusTone.Success else StatusTone.Accent)
            }
        }
        item {
            HolonSection("目标", eyebrow = item.focus ?: "WORK") {
                Text(item.objective ?: "没有目标说明", style = MaterialTheme.typography.bodyLarge)
                item.updatedAt?.let { SettingsValue("更新", it) }
                item.revision?.let { SettingsValue("版本", it.toString()) }
            }
        }
        item.resultSummary?.let { result ->
            item { HolonSection("结果", eyebrow = "RESULT") { Text(result) } }
        }
        item.blockedBy?.let { blocker ->
            item { HolonSection("阻塞", eyebrow = "NEEDS INPUT") { Text(blocker, color = MaterialTheme.colorScheme.error) } }
        }
        item.planArtifact?.let { plan ->
            item {
                HolonSection("计划", eyebrow = plan.relativePath ?: "PLAN") {
                    Text(
                        plan.preview ?: "计划文件可用，但没有内联预览。",
                        maxLines = 20,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (!plan.previewComplete) Text("预览已截断", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        if (item.todoList.isNotEmpty()) {
            item {
                HolonSection("待办", eyebrow = "${item.todoList.count { it.state == "completed" }}/${item.todoList.size}") {
                    item.todoList.forEach { todo ->
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Text(if (todo.state == "completed") "✓" else "○", color = if (todo.state == "completed") MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
                            Text(todo.text, modifier = Modifier.weight(1f))
                        }
                    }
                }
            }
        }
        if (item.workRefs.isNotEmpty()) {
            item {
                HolonSection("相关工作", eyebrow = "${item.workRefs.size} REFS") {
                    item.workRefs.take(20).forEach { ref ->
                        Text(ref.title ?: ref.ref, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        Text(ref.kind, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
        }
        if (state.workItemsBusy) item { CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp) }
    }
}

@Composable
private fun WorkspaceBrowserScreen(state: HolonUiState, viewModel: HolonViewModel, modifier: Modifier) {
    val context = LocalContext.current
    val prepared = state.preparedArtifact
    val listState = rememberLazyListState()
    val previewIndex = 2 + if (state.workspaces.size > 1) state.workspaces.size else 0
    LaunchedEffect(prepared?.localPath) {
        if (prepared != null) listState.animateScrollToItem(previewIndex)
    }
    val saveFile = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("*/*")) { target ->
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
        state = listState,
        modifier = modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(14.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text("文件", style = MaterialTheme.typography.headlineSmall)
                Text("从 Holon host 的受权 workspace 读取", color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        if (state.workspaces.size > 1) {
            items(state.workspaces, key = { "${it.workspaceId}:${it.executionRootId}" }) { workspace ->
                val selected = workspace == state.selectedWorkspace
                OutlinedButton(
                    onClick = { viewModel.selectWorkspace(workspace) },
                    modifier = Modifier.fillMaxWidth(),
                    border = BorderStroke(1.dp, if (selected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.outlineVariant),
                ) {
                    Text("${if (workspace.isActive) "当前 · " else ""}${workspace.label}", maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
        }
        state.selectedWorkspace?.let { workspace ->
            item {
                Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(8.dp)) {
                    Row(Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                        TextButton(onClick = viewModel::navigateWorkspaceUp, enabled = !state.workspaceDirectory?.path.isNullOrBlank()) { Text("↑ 上级") }
                        Column(Modifier.weight(1f)) {
                            Text(workspace.label, style = MaterialTheme.typography.labelLarge)
                            Text("/${state.workspaceDirectory?.path.orEmpty()}", style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        }
                    }
                }
            }
        }
        if (state.workspaceBusy) {
            item { Box(Modifier.fillMaxWidth().padding(20.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
        }
        prepared?.let { artifact ->
            item {
                HolonSection(artifact.fileName, eyebrow = artifact.mediaType) {
                    ArtifactPreview(artifact)
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        TextButton(onClick = { saveFile.launch(artifact.fileName) }) { Text("下载") }
                        TextButton(onClick = { shareArtifact(context, artifact) }) { Text("分享") }
                        TextButton(onClick = viewModel::clearPreparedArtifact) { Text("关闭") }
                    }
                }
            }
        }
        items(state.workspaceDirectory?.entries.orEmpty(), key = HolonWorkspaceEntry::name) { entry ->
            Surface(
                color = MaterialTheme.colorScheme.surface,
                border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                shape = RoundedCornerShape(8.dp),
                modifier = Modifier.fillMaxWidth().clickable { viewModel.openWorkspaceEntry(entry.name, entry.type == "directory") },
            ) {
                Row(Modifier.padding(horizontal = 12.dp, vertical = 11.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(if (entry.type == "directory") "▸" else "·", color = MaterialTheme.colorScheme.primary, modifier = Modifier.width(22.dp))
                    Column(Modifier.weight(1f)) {
                        Text(entry.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        if (entry.type != "directory") Text(
                            listOfNotNull(entry.mediaType, formatBytes(entry.size)).joinToString(" · "),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Text(if (entry.type == "directory") "›" else "预览", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        if (!state.workspaceBusy && state.workspaces.isEmpty()) item { EmptyPage("没有可浏览的 workspace", "Agent 尚未连接可访问的工作区。") }
        item { Spacer(Modifier.height(20.dp)) }
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
                            TextButton(
                                onClick = { viewModel.removeAttachment(index) },
                                enabled = !state.enqueueing,
                            ) { Text("移除") }
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
                enabled = !state.enqueueing,
                modifier = Modifier.fillMaxWidth(),
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = onImage, enabled = !state.enqueueing) { Text("相册") }
                TextButton(onClick = onCamera, enabled = !state.enqueueing) { Text("拍照") }
                TextButton(onClick = onFile, enabled = !state.enqueueing) { Text("文件") }
                Spacer(Modifier.weight(1f))
                Button(
                    onClick = viewModel::send,
                    enabled =
                        !state.enqueueing &&
                            (state.draft.isNotBlank() || state.attachments.isNotEmpty()),
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
                    Text("产物与关联工作", style = MaterialTheme.typography.headlineSmall)
                    Text(brief.createdAt, style = MaterialTheme.typography.labelSmall)
                }
                StatusPill("结果", StatusTone.Success)
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
