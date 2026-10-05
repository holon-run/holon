@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import run.holon.android.sdk.AgentSummary

@Composable
internal fun AgentsScreen(state: HolonUiState, viewModel: AgentsActions) {
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
                        operatorPreview = state.operatorPreviews[agent.id],
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
    operatorPreview: OperatorPreview? = null,
    onClick: () -> Unit,
) {
    val tone = agent.statusTone()
    val briefPreview = plainTextPreview(agent.latestBrief?.preview.orEmpty())
    val inputPreview = agent.inputPreview(operatorPreview)?.let(::plainTextPreview)
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
                    !inputPreview.isNullOrBlank() -> inputPreview
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
                            (operatorPreview?.createdAt.takeIf { inputPreview != null } ?: agent.latestBrief?.createdAt)?.let(::relativeTime),
                            agent.currentWorkItemId?.let { ui("有进行中的 WorkItem") },
                        ).joinToString(" · ").ifBlank { ui(if (inputPreview != null) "已提交输入" else "尚无活动") },
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
