package run.holon.android.app

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.BearerTokenProvider
import run.holon.android.sdk.CompatibilityResult
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonProtocolException
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonConversationStreamEvent
import run.holon.android.sdk.HolonTaskOutputSnapshot
import run.holon.android.sdk.HolonTaskSnapshot
import run.holon.android.sdk.HolonToolExecutionSnapshot
import run.holon.android.sdk.HolonWorkItemSnapshot
import run.holon.android.sdk.SseReconnectPolicy
import run.holon.android.sdk.SessionCredentialStore
import java.io.IOException

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                HolonApp(applicationContext)
            }
        }
    }
}

private enum class ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Failed,
}

private val requiredCapabilities = setOf("agents.conversation-read.v1")
private const val APP_PREFERENCES = "holon_android"
private const val BASE_URL_KEY = "base_url"

private fun connectionError(error: Throwable): String =
    when (error) {
        is HolonHttpException ->
            when (error.statusCode) {
                401, 403 -> "会话无效或无权限，请重新建立会话"
                else -> "服务端请求失败（HTTP ${error.statusCode}）"
            }
        is HolonProtocolException -> "服务端响应无效，请检查服务端版本"
        is IOException -> "网络连接失败，请检查服务端地址与网络"
        is IllegalArgumentException -> "服务端地址无效"
        else -> "连接失败，请重试"
    }

private fun isTerminalTaskStatus(status: String): Boolean =
    status.lowercase() in
        setOf("completed", "succeeded", "success", "failed", "error", "cancelled", "canceled", "done")

private fun isNetworkAvailable(context: Context): Boolean {
    val manager = context.getSystemService(ConnectivityManager::class.java) ?: return false
    val network = manager.activeNetwork ?: return false
    return manager.getNetworkCapabilities(network) != null
}

@Composable
private fun HolonApp(context: Context) {
    val scope = rememberCoroutineScope()
    val sessionStore = remember(context) { createSessionStore(context) }
    val preferences = remember(context) {
        context.getSharedPreferences(APP_PREFERENCES, Context.MODE_PRIVATE)
    }
    var baseUrl by remember {
        mutableStateOf(preferences.getString(BASE_URL_KEY, null) ?: defaultBaseUrl())
    }
    var sessionCredential by remember {
        mutableStateOf(sessionStore.read().orEmpty())
    }
    var networkAvailable by remember { mutableStateOf(isNetworkAvailable(context)) }
    var state by remember { mutableStateOf(ConnectionState.Disconnected) }
    var status by remember { mutableStateOf("未连接") }
    var agents by remember { mutableStateOf(emptyList<AgentSummary>()) }
    var requestGeneration by remember { mutableStateOf(0) }
    var logoutInProgress by remember { mutableStateOf(false) }
    var selectedAgent by remember { mutableStateOf<AgentSummary?>(null) }
    var conversation by remember { mutableStateOf<HolonConversationSnapshot?>(null) }
    var conversationStatus by remember { mutableStateOf("请选择 agent 查看会话") }
    var conversationCursor by remember { mutableStateOf<String?>(null) }
    var conversationResetRequired by remember { mutableStateOf(false) }
    var connectionJob by remember { mutableStateOf<kotlinx.coroutines.Job?>(null) }
    var conversationJob by remember { mutableStateOf<kotlinx.coroutines.Job?>(null) }
    var taskInput by remember { mutableStateOf("") }
    var taskStatus by remember { mutableStateOf("暂无任务") }
    var tasks by remember { mutableStateOf(emptyList<HolonTaskSnapshot>()) }
    var taskOutput by remember { mutableStateOf<HolonTaskOutputSnapshot?>(null) }
    var workItems by remember { mutableStateOf(emptyList<HolonWorkItemSnapshot>()) }
    var toolExecutions by remember { mutableStateOf(emptyList<HolonToolExecutionSnapshot>()) }
    var artifactStatus by remember { mutableStateOf<String?>(null) }
    var artifactContent by remember { mutableStateOf<String?>(null) }
    var taskRefreshJob by remember { mutableStateOf<kotlinx.coroutines.Job?>(null) }
    var taskSubmitJob by remember { mutableStateOf<kotlinx.coroutines.Job?>(null) }
    var restoreConversation by remember { mutableStateOf(false) }
    fun clearTaskDetails(cancelSubmit: Boolean = false) {
        taskRefreshJob?.cancel()
        if (cancelSubmit) {
            taskSubmitJob?.cancel()
        }
        tasks = emptyList()
        workItems = emptyList()
        taskOutput = null
        taskStatus = "暂无任务"
        artifactStatus = null
        artifactContent = null
    }

    fun fail(message: String) {
        state = ConnectionState.Failed
        status = message
        agents = emptyList()
        selectedAgent = null
        conversation = null
        toolExecutions = emptyList()
        artifactStatus = null
        artifactContent = null
        conversationJob?.cancel()
        clearTaskDetails(cancelSubmit = true)
    }

    fun client(): HolonHttpClient =
        HolonHttpClient(
            baseUrl = baseUrl,
            bearerTokenProvider = BearerTokenProvider { sessionStore.read() },
            sessionCredentialStore = sessionStore,
            insecureHttpHosts = if (BuildConfig.DEBUG) {
                setOf("10.0.2.2")
            } else {
                emptySet()
            },
        )

    fun refreshAgentDetails(
        agent: AgentSummary,
        generation: Int = requestGeneration,
        preferredTaskId: String? = null,
        loadLatestOutput: Boolean = true,
    ) {
        taskRefreshJob?.cancel()
        tasks = emptyList()
        workItems = emptyList()
        taskOutput = null
        taskStatus = "正在加载任务与 WorkItem…"
        taskRefreshJob =
            scope.launch {
                try {
                    val details = withContext(Dispatchers.IO) {
                        val connection = client()
                        val loadedTasks = connection.taskSnapshots(agent.id, limit = 20)
                        val loadedWorkItems = connection.workItemSnapshots(agent.id, limit = 20)
                        val preferredTask =
                            preferredTaskId?.let { taskId ->
                                loadedTasks.firstOrNull { it.taskId == taskId }
                                    ?: runCatching {
                                        connection.taskStatusSnapshot(agent.id, taskId)
                                    }.getOrNull()
                            }
                        val displayTasks =
                            if (preferredTask != null && loadedTasks.none { it.taskId == preferredTask.taskId }) {
                                listOf(preferredTask) + loadedTasks
                            } else {
                                loadedTasks
                            }
                        val latestOutput =
                            if (!loadLatestOutput) {
                                null
                            } else {
                                preferredTask ?: loadedTasks.firstOrNull()
                            }?.let { task ->
                                runCatching {
                                    connection.taskOutputSnapshot(agent.id, task.taskId)
                                }.getOrNull()
                            }
                        Triple(displayTasks, loadedWorkItems, latestOutput)
                    }
                    if (generation != requestGeneration) return@launch
                    val (loadedTasks, loadedWorkItems, latestOutput) = details
                    tasks = loadedTasks
                    workItems = loadedWorkItems
                    taskOutput = latestOutput
                    taskStatus =
                        if (latestOutput != null && isTerminalTaskStatus(latestOutput.status)) {
                            "任务已结束：${latestOutput.status}"
                        } else if (loadedTasks.isEmpty()) {
                            "暂无活动任务"
                        } else {
                            "活动任务：${loadedTasks.size}"
                        }
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Throwable) {
                    if (generation == requestGeneration) {
                        taskStatus = "任务/WorkItem 加载失败：${connectionError(error)}"
                        tasks = emptyList()
                        workItems = emptyList()
                        taskOutput = null
                    }
                }
            }
    }

    fun sendTask(agent: AgentSummary) {
        val text = taskInput.trim()
        if (text.isEmpty() || taskSubmitJob?.isActive == true) return
        taskStatus = "正在提交文字任务…"
        val generation = requestGeneration
        val existingTaskIds = tasks.mapTo(mutableSetOf()) { it.taskId }
        taskSubmitJob =
            scope.launch {
                try {
                    val result =
                        withContext(Dispatchers.IO) {
                            client().enqueueText(agent.id, text)
                        }
                    if (generation != requestGeneration) return@launch
                    if (!result.ok) {
                        throw HolonProtocolException("服务端未接受文字任务")
                    }
                    taskInput = ""
                    taskStatus =
                        if (result.messageId == null) {
                            "任务已排队"
                        } else {
                            "任务已排队：${result.messageId}"
                        }
                    var observedTaskId: String? = null
                    for (attempt in 0 until 120) {
                        if (observedTaskId == null) {
                            val newTask =
                                withContext(Dispatchers.IO) {
                                    client().taskSnapshots(agent.id, limit = 20)
                                        .firstOrNull { it.taskId !in existingTaskIds }
                                }
                            if (newTask != null) {
                                observedTaskId = newTask.taskId
                                tasks = listOf(newTask) + tasks.filter { it.taskId != newTask.taskId }
                                taskStatus = "任务状态：${newTask.status}"
                            }
                        }
                        val taskId = observedTaskId
                        if (taskId != null) {
                            val snapshot =
                                withContext(Dispatchers.IO) {
                                    client().taskStatusSnapshot(agent.id, taskId)
                                }
                            tasks = listOf(snapshot) + tasks.filter { it.taskId != taskId }
                            taskStatus = "任务状态：${snapshot.status}"
                            if (isTerminalTaskStatus(snapshot.status)) {
                                taskOutput =
                                    withContext(Dispatchers.IO) {
                                        runCatching {
                                            client().taskOutputSnapshot(agent.id, taskId)
                                        }.getOrNull()
                                    }
                                break
                            }
                        }
                        delay(500)
                    }
                    if (generation == requestGeneration) {
                        refreshAgentDetails(
                            agent = agent,
                            generation = generation,
                            preferredTaskId = observedTaskId,
                            loadLatestOutput = observedTaskId != null,
                        )
                        if (observedTaskId == null) {
                            taskStatus = "任务已排队，等待服务端生成 task"
                        }
                    }
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Throwable) {
                    if (generation == requestGeneration) {
                        taskStatus = "任务提交失败：${connectionError(error)}"
                    }
                }
            }
    }

    fun connect(preserveSelection: Boolean = false) {
        if (logoutInProgress || state == ConnectionState.Connecting) return
        if (!networkAvailable) {
            state = ConnectionState.Disconnected
            status = "网络离线，恢复网络后可自动重连"
            return
        }
        requestGeneration++
        val generation = requestGeneration
        if (BuildConfig.DEBUG) {
            if (sessionCredential.isBlank()) {
                sessionStore.clear()
            } else {
                sessionStore.write(sessionCredential)
            }
        }
        state = ConnectionState.Connecting
        status = "正在连接…"
        agents = emptyList()
        if (!preserveSelection) {
            selectedAgent = null
            conversation = null
        }
        toolExecutions = emptyList()
        artifactStatus = null
        artifactContent = null
        clearTaskDetails(cancelSubmit = true)
        conversationCursor = null
        conversationResetRequired = false
        conversationJob?.cancel()
        val url = baseUrl
        connectionJob = scope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    val connection = HolonHttpClient(
                        baseUrl = url,
                        bearerTokenProvider = BearerTokenProvider { sessionStore.read() },
                        sessionCredentialStore = sessionStore,
                        insecureHttpHosts = if (BuildConfig.DEBUG) setOf("10.0.2.2") else emptySet(),
                    )
                    val compatibility = connection.handshake(requiredCapabilities)
                    compatibility to if (compatibility is CompatibilityResult.Compatible) {
                        connection.listAgents()
                    } else {
                        emptyList()
                    }
                }
            }.onSuccess { (compatibility, agentList) ->
                if (generation != requestGeneration) return@onSuccess
                when (compatibility) {
                    is CompatibilityResult.Compatible -> {
                        state = ConnectionState.Connected
                        status =
                            "已连接 · ${compatibility.server.authMode} · ${agentList.size} agents"
                        agents = agentList
                    }
                    is CompatibilityResult.UnsupportedProtocol ->
                        fail("协议不兼容：${compatibility.actualName}/${compatibility.actualVersion}")
                    is CompatibilityResult.MissingCapabilities ->
                        fail("缺少能力：${compatibility.capabilities.joinToString()}")
                    CompatibilityResult.RejectedHandshake ->
                        fail("服务端拒绝 handshake")
                }
            }.onFailure { error ->
                if (generation == requestGeneration) {
                    if (error is HolonHttpException && error.statusCode == 401) {
                        sessionCredential = ""
                        sessionStore.clear()
                    }
                    fail(connectionError(error))
                }
            }
        }
    }

    fun loadConversation(agent: AgentSummary, reset: Boolean = false) {
        clearTaskDetails(cancelSubmit = true)
        selectedAgent = agent
        if (reset) {
            conversation = null
            conversationCursor = null
            conversationResetRequired = false
        }
        conversationStatus = "正在加载会话…"
        conversationJob?.cancel()
        val generation = requestGeneration
        conversationJob =
            scope.launch {
                runCatching {
                    withContext(Dispatchers.IO) {
                        val connection = client()
                        val snapshot = connection.conversationSnapshot(agent.id, limit = 30)
                        val toolIds =
                            snapshot.turns
                                .flatMap { turn ->
                                    (turn.raw["tool_execution_ids"] as? JsonArray)
                                        .orEmpty()
                                        .mapNotNull { it.jsonPrimitive.contentOrNull }
                                }
                                .distinct()
                                .takeLast(20)
                        val tools =
                            toolIds.mapNotNull { toolId ->
                                runCatching {
                                    connection.toolExecutionSnapshot(agent.id, toolId)
                                }.getOrNull()
                            }
                        snapshot to tools
                    }
                }.onSuccess { snapshot ->
                    if (generation != requestGeneration) return@onSuccess
                    val (loadedSnapshot, loadedTools) = snapshot
                    conversation = loadedSnapshot
                    toolExecutions = loadedTools
                    artifactStatus = null
                    artifactContent = null
                    conversationCursor = loadedSnapshot.snapshotCursor
                    conversationResetRequired = false
                    conversationStatus = "已加载 ${loadedSnapshot.turns.size} 个单元，正在接收增量…"
                    refreshAgentDetails(agent, generation)
                    conversationJob =
                        scope.launch {
                            runCatching {
                                withContext(Dispatchers.IO) {
                                    client().reconnectingConversationChanges(
                                        agentId = agent.id,
                                        after = loadedSnapshot.snapshotCursor,
                                        policy = SseReconnectPolicy(maxAttempts = 8),
                                    ).forEach { change ->
                                        when (change) {
                                            is HolonConversationStreamEvent.Checkpoint ->
                                                conversationCursor = change.checkpoint
                                            is HolonConversationStreamEvent.ResetRequired -> {
                                                conversationResetRequired = true
                                                conversationStatus =
                                                    "服务端要求重新同步：${change.reason ?: "cursor_invalid"}"
                                                return@withContext
                                            }
                                            else -> Unit
                                        }
                                    }
                                }
                            }.onFailure { error ->
                                if (generation == requestGeneration) {
                                    conversationStatus = "会话流已断开：${connectionError(error)}"
                                }
                            }
                        }
                }.onFailure { error ->
                    if (generation == requestGeneration) {
                        conversationStatus = "会话加载失败：${connectionError(error)}"
                    }
                }
            }
    }

    fun logout() {
        if (logoutInProgress) return
        requestGeneration++
        val logoutGeneration = requestGeneration
        logoutInProgress = true
        state = ConnectionState.Disconnected
        status = "已登出"
        agents = emptyList()
        selectedAgent = null
        conversation = null
        toolExecutions = emptyList()
        artifactStatus = null
        artifactContent = null
        conversationJob?.cancel()
        clearTaskDetails(cancelSubmit = true)
        sessionCredential = ""
        scope.launch {
            runCatching { withContext(Dispatchers.IO) { client().logout() } }
            if (logoutGeneration == requestGeneration) {
                sessionStore.clear()
                logoutInProgress = false
            }
        }
    }

    fun loadArtifact(agent: AgentSummary, tool: HolonToolExecutionSnapshot, index: Int) {
        artifactStatus = "正在加载产物…"
        artifactContent = null
        val generation = requestGeneration
        scope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    client().artifact(agent.id, tool.toolExecutionId, index)
                }
            }.onSuccess { artifact ->
                if (generation == requestGeneration && selectedAgent?.id == agent.id) {
                    artifactStatus = "产物 #${artifact.artifactIndex} · ${artifact.size} bytes"
                    artifactContent = artifact.content
                }
            }.onFailure { error ->
                if (generation == requestGeneration && selectedAgent?.id == agent.id) {
                    artifactStatus = "产物加载失败：${connectionError(error)}"
                }
            }
        }
    }

    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(context) {
        val manager = context.getSystemService(ConnectivityManager::class.java)
        if (manager == null) {
            onDispose { }
        } else {
            val callback =
                object : ConnectivityManager.NetworkCallback() {
                    override fun onAvailable(network: Network) {
                        scope.launch {
                            networkAvailable = true
                            if (sessionCredential.isNotBlank() &&
                                state != ConnectionState.Connected &&
                                state != ConnectionState.Connecting
                            ) {
                                connect(preserveSelection = true)
                            }
                        }
                    }

                    override fun onLost(network: Network) {
                        scope.launch {
                            networkAvailable = isNetworkAvailable(context)
                            if (!networkAvailable || state == ConnectionState.Connecting) {
                                requestGeneration++
                                connectionJob?.cancel()
                                conversationJob?.cancel()
                                conversation = null
                                conversationCursor = null
                                restoreConversation = selectedAgent != null
                                clearTaskDetails(cancelSubmit = true)
                                state = ConnectionState.Disconnected
                                status = "网络离线，恢复网络后自动重连"
                                conversationStatus = "网络离线，前台恢复后重新同步"
                            }
                        }
                    }
                }
            manager.registerDefaultNetworkCallback(callback)
            onDispose {
                manager.unregisterNetworkCallback(callback)
            }
        }
    }

    DisposableEffect(lifecycleOwner) {
        val observer =
            LifecycleEventObserver { _, event ->
                when (event) {
                    Lifecycle.Event.ON_STOP -> {
                        if (state != ConnectionState.Disconnected ||
                            connectionJob?.isActive == true ||
                            conversationJob?.isActive == true ||
                            taskRefreshJob?.isActive == true ||
                            taskSubmitJob?.isActive == true
                        ) {
                            requestGeneration++
                            connectionJob?.cancel()
                            conversationJob?.cancel()
                            conversation = null
                            conversationCursor = null
                            restoreConversation = true
                            clearTaskDetails(cancelSubmit = true)
                            state = ConnectionState.Disconnected
                            status = "应用进入后台，前台恢复后重新连接"
                            conversationStatus = "应用进入后台，前台恢复后重新同步"
                        }
                    }
                    Lifecycle.Event.ON_START -> {
                        if (networkAvailable &&
                            sessionCredential.isNotBlank() &&
                            state != ConnectionState.Connected &&
                            state != ConnectionState.Connecting
                        ) {
                            connect(preserveSelection = true)
                        }
                    }
                    else -> Unit
                }
            }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    LaunchedEffect(Unit) {
        if (networkAvailable && sessionCredential.isNotBlank()) {
            connect()
        }
    }

    LaunchedEffect(state, restoreConversation) {
        if (state == ConnectionState.Connected && restoreConversation) {
            restoreConversation = false
            selectedAgent?.let { loadConversation(it) }
        }
    }

    Column(
        modifier =
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Holon", style = MaterialTheme.typography.headlineMedium)
        Text(
            text = when (state) {
                ConnectionState.Connected -> "连接状态：已连接"
                ConnectionState.Connecting -> "连接状态：连接中"
                ConnectionState.Failed -> "连接状态：失败"
                ConnectionState.Disconnected -> "连接状态：未连接"
            },
            style = MaterialTheme.typography.titleMedium,
        )
        Text(
            if (networkAvailable) {
                "网络状态：在线"
            } else {
                "网络状态：离线（请求已暂停，恢复后自动重连）"
            },
            color =
                if (networkAvailable) {
                    MaterialTheme.colorScheme.onSurface
                } else {
                    MaterialTheme.colorScheme.error
                },
        )
        OutlinedTextField(
            value = baseUrl,
            onValueChange = {
                baseUrl = it
                preferences.edit().putString(BASE_URL_KEY, it).apply()
            },
            label = { Text("Holon API base URL") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        if (BuildConfig.DEBUG) {
            OutlinedTextField(
                value = sessionCredential,
                onValueChange = {
                    sessionCredential = it
                    sessionStore.write(it)
                },
                label = { Text("Debug session credential") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                "仅 debug 构建显示。此处注入已兑换的 session credential，不代表正式登录流程。",
                style = MaterialTheme.typography.bodySmall,
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = ::connect,
                enabled = networkAvailable && state != ConnectionState.Connecting && !logoutInProgress,
            ) {
                Text(when (state) {
                    ConnectionState.Connecting -> "连接中…"
                    ConnectionState.Failed -> "重试"
                    else -> "连接"
                })
            }
            TextButton(onClick = ::logout, enabled = !logoutInProgress) {
                Text(if (logoutInProgress) "登出中…" else "登出")
            }
        }
        Text(status, color = if (state == ConnectionState.Failed) {
            MaterialTheme.colorScheme.error
        } else {
            MaterialTheme.colorScheme.onSurface
        })
        Spacer(Modifier.height(8.dp))
        Text("Agents", style = MaterialTheme.typography.titleLarge)
        when (state) {
            ConnectionState.Disconnected -> Text("连接后查看 agent")
            ConnectionState.Connecting -> Text("正在加载 agent…")
            ConnectionState.Failed -> Text("无法加载 agent，请检查提示后重试")
            ConnectionState.Connected -> if (agents.isEmpty()) {
                Text("暂无 agent")
            } else {
                agents.forEach { agent ->
                    Column {
                        Text(
                            "${agent.displayName} (${agent.id})" +
                                if (agent.isDefault) " · 默认" else "",
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Text("注册：${agent.registryStatus} · 运行：${agent.runtimeStatus} · 待处理：${agent.pending}")
                        TextButton(onClick = { loadConversation(agent) }) {
                            Text(if (selectedAgent?.id == agent.id) "已选择 · 查看会话" else "查看会话")
                        }
                    }
                }
            }
        }
        selectedAgent?.let { agent ->
            Spacer(Modifier.height(16.dp))
            Text("会话时间线 · ${agent.displayName}", style = MaterialTheme.typography.titleLarge)
            Text(
                conversationStatus,
                color =
                    if (conversationResetRequired) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurface
                    },
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = { loadConversation(agent, reset = true) }) {
                    Text(if (conversationResetRequired) "重新同步" else "刷新")
                }
                TextButton(
                    onClick = {
                        conversationJob?.cancel()
                        conversation = null
                        conversationCursor = null
                        conversationResetRequired = false
                        conversationStatus = "已重置本地 checkpoint"
                    },
                ) {
                    Text("重置")
                }
            }
            conversation?.turns?.forEach { turn ->
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 4.dp),
                ) {
                    Text(turn.id, style = MaterialTheme.typography.labelMedium)
                    Text(turn.summary)
                }
            }
            conversationCursor?.let {
                Text("checkpoint: $it", style = MaterialTheme.typography.labelSmall)
            }
            Text("工具调用", style = MaterialTheme.typography.titleMedium)
            if (toolExecutions.isEmpty()) {
                Text("暂无可见工具调用")
            } else {
                toolExecutions.forEach { tool ->
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(vertical = 4.dp),
                    ) {
                        Text(
                            "${tool.toolName} · ${tool.status}",
                            style = MaterialTheme.typography.labelMedium,
                        )
                        tool.summary?.let { summary -> Text(summary) }
                        repeat(tool.artifactCount) { index ->
                            TextButton(onClick = { loadArtifact(agent, tool, index) }) {
                                Text("查看产物 #$index")
                            }
                        }
                    }
                }
            }
            artifactStatus?.let { Text(it) }
            artifactContent?.let {
                Text(it, modifier = Modifier.fillMaxWidth())
            }
            Spacer(Modifier.height(12.dp))
            Text("文字任务", style = MaterialTheme.typography.titleMedium)
            Text(
                "图片任务：当前服务端未公开 Android 可用接口，已安全禁用。",
                style = MaterialTheme.typography.bodySmall,
            )
            OutlinedTextField(
                value = taskInput,
                onValueChange = { taskInput = it },
                label = { Text("输入任务") },
                modifier = Modifier.fillMaxWidth(),
                enabled = taskSubmitJob?.isActive != true && taskRefreshJob?.isActive != true,
            )
            Button(
                onClick = { sendTask(agent) },
                enabled =
                    taskInput.isNotBlank() &&
                        taskSubmitJob?.isActive != true &&
                        taskRefreshJob?.isActive != true,
            ) {
                Text(
                    if (taskSubmitJob?.isActive == true || taskRefreshJob?.isActive == true) {
                        "处理中…"
                    } else {
                        "发送任务"
                    },
                )
            }
            Text(taskStatus)
            tasks.forEach { task ->
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 4.dp),
                ) {
                    Text("${task.taskId} · ${task.status}", style = MaterialTheme.typography.labelMedium)
                    val summary = task.summary
                    if (summary != null) {
                        Text(summary)
                    }
                }
            }
            taskOutput?.let { output ->
                Text("最近结果 · ${output.status}", style = MaterialTheme.typography.titleSmall)
                val resultSummary = output.resultSummary
                if (resultSummary != null) {
                    Text(resultSummary)
                }
                val outputPreview = output.outputPreview
                if (!outputPreview.isNullOrBlank()) {
                    Text(outputPreview)
                }
            }
            Text("WorkItem（只读）", style = MaterialTheme.typography.titleMedium)
            if (workItems.isEmpty()) {
                Text("暂无可见 WorkItem")
            } else {
                workItems.forEach { item ->
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(vertical = 4.dp),
                    ) {
                        Text("${item.workItemId} · ${item.state}", style = MaterialTheme.typography.labelMedium)
                        val objective = item.objective
                        if (objective != null) {
                            Text(objective)
                        }
                    }
                }
            }
        }
    }
}
