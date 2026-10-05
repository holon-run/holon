@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.RadioButtonUnchecked
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import run.holon.android.sdk.HolonWorkItemSnapshot

@Composable
internal fun WorkItemsScreen(
    state: HolonUiState,
    viewModel: WorkActions,
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
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(ui("进行中的任务") + " · ${state.tasks.size}", style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                IconButton(onClick = viewModel::refreshTasks, enabled = !state.tasksBusy && state.online) { Icon(Icons.Default.Refresh, contentDescription = ui("刷新任务")) }
            }
            if (state.tasksBusy) LinearProgressIndicator(Modifier.fillMaxWidth())
            state.tasksError?.let { Text(ui(it), color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = 16.dp)) }
            if (state.tasks.isEmpty() && !state.tasksBusy && state.tasksError == null) Text(ui("暂无进行中的任务"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp))
        }
        items(state.tasks, key = { "task:${it.taskId}" }) { task ->
            TaskRow(task) { viewModel.openTask(task) }
        }
        item {
            HorizontalDivider(Modifier.padding(vertical = 8.dp))
            Text(ui("工作记录"), style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(horizontal = 16.dp))
        }
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
internal fun WorkItemDetailScreen(state: HolonUiState, viewModel: WorkActions, showBack: Boolean = true) {
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
