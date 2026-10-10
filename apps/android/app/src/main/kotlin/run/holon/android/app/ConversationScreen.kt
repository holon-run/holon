@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Image
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Share
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.verticalScroll
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
import androidx.compose.foundation.Image
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.saveable.rememberSaveable
import run.holon.android.sdk.HolonContentReportCategory
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonConversationTurn

@Composable
internal fun ConversationScreen(state: HolonUiState, viewModel: ConversationActions) {
    val agent = state.selectedAgent ?: return
    val planFile = state.planFile
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
    val isDetail = state.planFile != null || state.preparedArtifact != null || state.selectedWorkItem != null || state.selectedTask != null || state.selectedBrief != null || state.fullScreenTurn
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
                    planFile != null -> FileReaderScreen(
                        artifact = planFile,
                        title = ui("完整计划"),
                        onBack = viewModel::closePlanFile,
                        onSave = viewModel::saveArtifactToDevice,
                        onShare = { shareArtifact(context, planFile) },
                    )
                    state.preparedArtifact != null -> WorkspaceBrowserScreen(state, viewModel, Modifier.fillMaxSize(), filePosition)
                    state.selectedBrief != null && state.selectedWorkItem == null -> BriefScreen(state, viewModel)
                    wideWorkLayout && state.selectedTask == null -> {
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
                    state.selectedTask != null -> TaskDetailScreen(state, viewModel)
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
                            if (state.tasks.isNotEmpty()) TextButton(onClick = { viewModel.selectAgentSection(AgentSection.Work) }, modifier = Modifier.align(Alignment.Start)) {
                                Text(ui("进行中的任务") + " · ${state.tasks.size}", style = MaterialTheme.typography.labelMedium)
                            }
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
                AgentConversationRow(other, unreadCount = other.unreadCount(state.briefReadStates), operatorPreview = state.operatorPreviews[other.id], onClick = { agentChooser = false; viewModel.openAgent(other) })
            }
        }
    }
    if (state.reportTarget != null) ContentReportSheet(state, viewModel)
    if (modelChooser) ModelPickerSheet(
        state = state,
        onDismiss = { modelChooser = false },
        onRefresh = { viewModel.loadModelCatalog(refresh = true) },
        onSelect = { model, effort -> viewModel.setAgentModel(model, effort) },
        onAuto = viewModel::clearAgentModel,
    )
}


@Composable
internal fun ContentReportSheet(state: HolonUiState, viewModel: ConversationActions) {
    if (state.reportTarget == null) return
    val keyboard = LocalSoftwareKeyboardController.current
    ModalBottomSheet(
        onDismissRequest = { if (!state.reportSubmitting) viewModel.dismissContentReport() },
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        ContentReportForm(
            state = state,
            onCategorySelected = viewModel::selectReportCategory,
            onDescriptionChanged = viewModel::updateReportDescription,
            onDismiss = viewModel::dismissContentReport,
            onSubmit = {
                keyboard?.hide()
                viewModel.submitContentReport()
            },
        )
    }
}

@Composable
internal fun ContentReportForm(
    state: HolonUiState,
    onCategorySelected: (HolonContentReportCategory) -> Unit,
    onDescriptionChanged: (String) -> Unit,
    onDismiss: () -> Unit,
    onSubmit: () -> Unit,
) {
    Column(
        Modifier.fillMaxWidth().imePadding().navigationBarsPadding().padding(horizontal = 20.dp, vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Column(
            Modifier.fillMaxWidth().weight(1f, fill = false).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(ui("举报这条内容"), style = MaterialTheme.typography.titleMedium)
            Text(
                ui("选择最符合的原因，可附加说明。举报会提交到运行时的内容审核流程。"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            contentReportCategoryOptions.forEach { option ->
                Row(
                    modifier = Modifier.fillMaxWidth().clickable(enabled = !state.reportSubmitting) {
                        onCategorySelected(option.category)
                    },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    RadioButton(
                        selected = state.reportCategory == option.category,
                        onClick = { onCategorySelected(option.category) },
                        enabled = !state.reportSubmitting,
                    )
                    Text(ui(option.sourceLabel), style = MaterialTheme.typography.bodyMedium)
                }
            }
            OutlinedTextField(
                value = state.reportDescription,
                onValueChange = onDescriptionChanged,
                label = { Text(ui("补充说明（可选）")) },
                enabled = !state.reportSubmitting,
                minLines = 3,
                maxLines = 6,
                modifier = Modifier.fillMaxWidth(),
                supportingText = { Text("${state.reportDescription.length} / $CONTENT_REPORT_DESCRIPTION_LIMIT") },
            )
            state.reportError?.let {
                Text(
                    ui(it),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            OutlinedButton(
                onClick = onDismiss,
                enabled = !state.reportSubmitting,
                modifier = Modifier.weight(1f),
            ) {
                Text(ui("取消"))
            }
            Button(
                onClick = onSubmit,
                enabled = !state.reportSubmitting && state.reportCategory != null,
                modifier = Modifier.weight(1f),
            ) {
                if (state.reportSubmitting) {
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                } else {
                    Text(ui("提交举报"))
                }
            }
        }
        Spacer(Modifier.height(8.dp))
    }
}


internal class ConversationTimelinePosition(val listState: LazyListState = LazyListState()) {
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
internal fun ModelPickerSheet(
    state: HolonUiState,
    onDismiss: () -> Unit,
    onRefresh: () -> Unit,
    onSelect: (String, String?) -> Unit,
    onAuto: () -> Unit,
) {
    val agent = state.selectedAgent ?: return
    var search by remember(agent.id) { mutableStateOf("") }
    var showAll by remember(agent.id) { mutableStateOf(false) }
    var pendingModel by remember(agent.id) { mutableStateOf<String?>(null) }
    var effort by remember(agent.id, pendingModel) { mutableStateOf<String?>(null) }
    val options = state.modelCatalog?.options.orEmpty()
    val common = commonModelOptions(options, state.agents, agent.effectiveModel)
    val visibleOptions = if (search.isNotBlank()) options.filter { it.model.contains(search, true) || it.displayName.contains(search, true) }
        else if (showAll || common.isEmpty()) options else common
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
            if (search.isBlank() && common.isNotEmpty()) Row(verticalAlignment = Alignment.CenterVertically) {
                Text(ui(if (showAll) "全部模型" else "常用模型"), style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                TextButton(onClick = { showAll = !showAll }) { Text(ui(if (showAll) "仅显示常用" else "查看全部")) }
            }
            LazyColumn(
                Modifier.heightIn(max = 380.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                items(
                    visibleOptions,
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
internal fun ConversationTimeline(
    state: HolonUiState,
    viewModel: ConversationActions,
    modifier: Modifier,
    position: ConversationTimelinePosition,
) {
    val snapshot = state.conversation
    val listState = position.listState
    val dragging by listState.interactionSource.collectIsDraggedAsState()
    val scope = rememberCoroutineScope()
    val turns = run.holon.android.sdk.mergeConversationTurns(state.olderTurns, snapshot?.turns.orEmpty())
    val rows = conversationRows(turns, briefs = state.briefs)
    val latestBriefId = state.selectedAgent?.latestBrief?.briefId
    val latestBriefEventSeq = state.selectedAgent?.latestBrief?.createdEventSeq
    val agentId = state.selectedAgent?.id
    val pending = pendingConversationInputs(snapshot?.pendingInputs.orEmpty())
    var pendingExpanded by rememberSaveable(agentId) { mutableStateOf(false) }
    var selectedPendingId by rememberSaveable(agentId) { mutableStateOf<String?>(null) }
    val tail = "conversation-tail"
    val itemKeys = buildList {
        if (state.hasOlderTurns) add("load-older-turns")
        addAll(rows.map(ConversationRow::key))
        addAll(pending.keys)
        addAll(state.outbox.map { "outbox:${it.requestId}" })
        if (turns.isEmpty() && state.outbox.isEmpty() && pending.keys.isEmpty()) add("empty")
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
            items(pending.operator, key = { "pending:${it.messageId}" }) { input ->
                OperatorInputText(input.preview, input.createdAt, input.actorDisplayName, ui("待处理"))
            }
            if (pending.background.isNotEmpty()) item(key = "pending-background") {
                PendingMessagesCard(
                    inputs = pending.background,
                    expanded = pendingExpanded,
                    onToggle = {
                        position.followLatest = false
                        pendingExpanded = !pendingExpanded
                    },
                    onInput = { selectedPendingId = it.messageId },
                )
            }
            items(state.outbox, key = { "outbox:${it.requestId}" }) { message ->
                LocalMessageCard(message, retryEnabled = !state.enqueueing,
                    onRetry = { viewModel.retryMessage(message) },
                    onEdit = { viewModel.editFailedMessage(message) },
                    onRemove = { viewModel.removeFailedMessage(message) })
            }
            if (turns.isEmpty() && state.outbox.isEmpty() && pending.keys.isEmpty()) item(key = "empty") {
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
    pending.background.firstOrNull { it.messageId == selectedPendingId }?.let { input ->
        ModalBottomSheet(onDismissRequest = { selectedPendingId = null }) {
            PendingMessageDetails(input) { task ->
                selectedPendingId = null
                viewModel.openTask(task)
                viewModel.loadTaskOutput()
            }
        }
    }
}

@Composable
internal fun OperatorInput(turn: HolonConversationTurn) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        turn.inputs.filter { !it.interjected && (it.presentationClass == "operator" || (it.presentationClass == null && turn.presentationClass == "operator")) }.forEach { input ->
            OperatorInputText(input.preview, input.createdAt, input.actorDisplayName)
        }
    }
}

@Composable
internal fun OperatorInputText(text: String, createdAt: String?, actor: String?, status: String? = null) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(16.dp, 16.dp, 4.dp, 16.dp), modifier = Modifier.fillMaxWidth(0.92f)) {
            Column(Modifier.padding(horizontal = 13.dp, vertical = 10.dp)) {
                MarkdownText(text.ifBlank { ui("已提交输入") })
                listOfNotNull(actor, createdAt?.let(::relativeTime), status).takeIf { it.isNotEmpty() }?.let {
                    Text(it.joinToString(" · "), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
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
internal fun InlineTurnProcess(turn: HolonConversationTurn, state: HolonUiState, viewModel: ConversationActions, onInteraction: () -> Unit) {
    val expanded = state.selectedTurn?.id == turn.id
    val detail = state.conversationDetail.takeIf { expanded }
    val activities = detail?.activities.orEmpty().filter { it.kind != "operator" && !(it.kind == "assistant" && it.summary.isBlank()) }
    val openResult: (run.holon.android.sdk.HolonTaskSnapshot) -> Unit = { task -> viewModel.openTask(task); viewModel.loadTaskOutput() }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        TurnProcessHeader(turn, expanded) { onInteraction(); if (expanded) viewModel.closeTurn() else viewModel.openTurn(turn) }
        if (expanded) {
            if (detail == null && state.detailBusy) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
            if (detail == null && !state.detailBusy) TextButton(onClick = { viewModel.openTurn(turn) }) { Text(ui("重试")) }
            detail?.let {
                if (it.coverageKind != "complete") Text(detailCoverageMessage(it.coverageKind, it.coverageReason), style = MaterialTheme.typography.bodySmall)
                if (it.hasMore || activities.size > 6) Text(ui("显示最近过程，更多内容可全屏查看"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        turnProcessRows(turn, if (expanded) activities.takeLast(6) else emptyList()).forEach { row ->
            when (row) {
                is TurnProcessRow.TaskResult -> if (expanded) TaskResultProcessRow(row.input, row.input.createdAt ?: turn.startedAt, openResult)
                is TurnProcessRow.Interjection -> {
                    if (row.input.presentationClass == "operator" || row.input.presentationClass == null && turn.presentationClass == "operator")
                        OperatorInputText(row.input.preview, row.input.createdAt, row.input.actorDisplayName, ui("补充输入"))
                    else Text(row.input.preview, style = MaterialTheme.typography.bodySmall)
                }
                is TurnProcessRow.Activity -> {
                    val activity = row.activity
                    val open = state.selectedActivity?.id == activity.id
                    ActivityRow(activity = activity, expanded = open, detail = state.selectedToolExecution.takeIf { open },
                        loading = open && state.detailBusy,
                        onReport = contentReportAction(activity, state.selectedAgent?.id, turn.id, viewModel::beginContentReport),
                        onOpen = { onInteraction(); if (open) viewModel.closeActivity() else viewModel.inspectActivity(activity) })
                }
            }
        }
        // Older summaries have no canonical interjection key, but their input stays readable.
        turn.inputs.filter { it.interjected && it.activityKey == null && it.taskResult == null && (it.presentationClass == "operator" || it.presentationClass == null && turn.presentationClass == "operator") }.forEach { input ->
            OperatorInputText(input.preview, input.createdAt, input.actorDisplayName, ui("补充输入"))
        }
        if (expanded) TextButton(onClick = { viewModel.setTurnFullScreen(true) }) { Text(ui("全屏查看过程")) }
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
    executionKind == "active" && completedAt == null

internal fun HolonConversationTurn.compactStatusText(): String? =
    when {
        isRunning() -> ui("执行中")
        terminalOutcome == "provider_failed_needs_recovery" ||
            resultKind.contains("failure", true) -> ui("本轮失败")
        terminalOutcome in setOf("aborted", "interrupted", "baseline_over_budget") -> ui("本轮未完成")
        briefIds.isNotEmpty() || resultKind == "available" -> ui("结果载入中")
        resultKind == "unavailable" -> ui("结果暂不可用")
        taskResultHeader() != null -> taskLabel(taskResultHeader()!!.taskResult!!.status)
        resultKind == "none" -> ui("没有结果摘要")
        else -> null
    }

internal fun HolonConversationTurn.exceptionStatus(): Pair<String, StatusTone>? =
    when {
        attentionKind != null -> ui("需注意") to StatusTone.Warning
        resultKind.contains("failure", true) || terminalOutcome == "failure" -> ui("失败") to StatusTone.Danger
        else -> null
    }

@Composable
internal fun TurnDetailScreen(state: HolonUiState, viewModel: ConversationActions) {
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
    val processRows = turnProcessRows(turn, activities)
    LaunchedEffect(turn.id, latestActivityRevision, processRows.size) {
        if (activities.isNotEmpty() && latestActivityRevision != null) {
            val inputCount = turn.inputs.count { it.presentationClass != "internal" && (!it.interjected || it.activityKey == null) }
            val coverageCount = if (detail?.coverageKind != null && detail.coverageKind != "complete") 1 else 0
            val historyCount = if (detail?.hasMore == true) 1 else 0
            val latestIndex = 1 + inputCount + coverageCount + historyCount + processRows.indexOfLast { it is TurnProcessRow.Activity }
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
            turn.inputs.filter { it.taskResult == null && it.presentationClass != "internal" && (!it.interjected || it.activityKey == null) }.forEach { input ->
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
            turn.inputs.filter { it.taskResult != null && !it.interjected }.forEach { input ->
                item(key = "input:${input.messageId}") { TaskResultProcessRow(input, input.createdAt ?: turn.startedAt) { task -> viewModel.openTask(task); viewModel.loadTaskOutput() } }
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
                items(processRows.filterNot { it is TurnProcessRow.TaskResult && !it.input.interjected }, key = TurnProcessRow::key) { row ->
                    if (row is TurnProcessRow.TaskResult) {
                        TaskResultProcessRow(row.input, row.input.createdAt) { task -> viewModel.openTask(task); viewModel.loadTaskOutput() }
                        return@items
                    }
                    if (row is TurnProcessRow.Interjection) {
                        Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(8.dp)) {
                            Column(Modifier.fillMaxWidth().padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                Text(ui("补充输入") + row.input.actorDisplayName?.let { " · $it" }.orEmpty(), style = MaterialTheme.typography.labelSmall)
                                Text(row.input.preview, style = MaterialTheme.typography.bodyMedium)
                            }
                        }
                        return@items
                    }
                    val activity = (row as TurnProcessRow.Activity).activity
                    val expanded = state.selectedActivity?.id == activity.id
                    ActivityRow(
                        activity = activity,
                        expanded = expanded,
                        detail = state.selectedToolExecution.takeIf { expanded },
                        loading = expanded && state.detailBusy,
                        onReport = contentReportAction(activity, state.selectedAgent?.id, turn.id, viewModel::beginContentReport),
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

internal fun detailCoverageMessage(kind: String, reason: String?): String {
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
    onReport: (() -> Unit)? = null,
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
                if (!isTool && onReport != null) {
                    var reportMenuOpen by remember(activity.id) { mutableStateOf(false) }
                    Box {
                        IconButton(
                            onClick = { reportMenuOpen = true },
                            modifier = Modifier.size(32.dp),
                        ) {
                            Icon(Icons.Default.MoreVert, contentDescription = ui("消息操作"), modifier = Modifier.size(18.dp))
                        }
                        DropdownMenu(expanded = reportMenuOpen, onDismissRequest = { reportMenuOpen = false }) {
                            DropdownMenuItem(
                                text = { Text(ui("举报")) },
                                onClick = {
                                    reportMenuOpen = false
                                    onReport()
                                },
                            )
                        }
                    }
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
                            detail.summary?.takeIf { it.isNotBlank() && it != activity.summary && payloadBlocks.none { block -> block.text == it } }?.let {
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
internal fun ActivityDetailScreen(state: HolonUiState, viewModel: ConversationActions) {
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
internal fun ToolPayloadPreview(title: String, payload: String, code: Boolean = true, limit: Int = 2_000) {
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
internal fun LocalMessageCard(
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
internal fun Composer(
    state: HolonUiState,
    viewModel: ConversationActions,
    onImage: () -> Unit,
    onFile: () -> Unit,
    onCamera: () -> Unit,
) {
    val focus = LocalFocusManager.current
    val keyboard = LocalSoftwareKeyboardController.current
    MessageComposer(
        draft = state.draft,
        attachments = state.attachments,
        sending = state.enqueueing,
        staging = state.stagingAttachment,
        canStop = state.selectedAgent?.currentRunId != null,
        stopping = state.abortingRun,
        onDraft = viewModel::updateDraft,
        onSend = { focus.clearFocus(); keyboard?.hide(); viewModel.send() },
        onStop = viewModel::stopCurrentTurn,
        onRemove = viewModel::removeAttachment,
        onImage = onImage,
        onFile = onFile,
        onCamera = onCamera,
    )
}

@Composable
internal fun BriefScreen(state: HolonUiState, viewModel: ConversationActions) {
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

internal fun shareBrief(context: Context, text: String) {
    val intent = Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_TEXT, text)
    }
    context.startActivity(Intent.createChooser(intent, ui("分享结果")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
}

@Composable
internal fun TurnProcessHeader(turn: HolonConversationTurn, expanded: Boolean, onToggle: () -> Unit) {
    Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).clickable(onClick = onToggle).padding(vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore, contentDescription = null, modifier = Modifier.size(18.dp))
        Spacer(Modifier.width(6.dp))
        val task = turn.taskResultHeader()?.taskResult
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(task?.summary ?: if (task?.responseMessageId != null) ui("收到 Agent 回复") else ui(if (task != null) "收到任务结果" else "本轮过程"), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (task != null) {
                Text(taskResultStatus(task), style = MaterialTheme.typography.labelSmall, color = if (task.status in setOf("failed", "interrupted")) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)
                if (task.status in setOf("failed", "interrupted") && task.responseMessageId == null && task.preview.isNotBlank())
                    Text(taskResultFailureReason(task.preview), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        Spacer(Modifier.width(8.dp))
        val status = turn.exceptionStatus()?.first ?: if (turn.isRunning()) ui("执行中") else if (turn.briefIds.isEmpty() && turn.taskResultHeader() == null) turn.compactStatusText() else null
        status?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
}
