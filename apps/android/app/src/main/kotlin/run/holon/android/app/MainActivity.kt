package run.holon.android.app

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
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
        enableEdgeToEdge(
            statusBarStyle = androidx.activity.SystemBarStyle.light(
                android.graphics.Color.TRANSPARENT,
                android.graphics.Color.TRANSPARENT,
            ),
            navigationBarStyle = androidx.activity.SystemBarStyle.light(
                android.graphics.Color.TRANSPARENT,
                android.graphics.Color.TRANSPARENT,
            ),
        )
        setContent {
            HolonTheme {
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
    var showConnectionEditor by remember { mutableStateOf(false) }
    var networkAvailable by remember { mutableStateOf(isNetworkAvailable(context)) }
    var state by remember { mutableStateOf(ConnectionState.Disconnected) }
    var status by remember { mutableStateOf("未连接") }
    var agents by remember { mutableStateOf(emptyList<AgentSummary>()) }
    var requestGeneration by remember { mutableStateOf(0) }
    var logoutInProgress by remember { mutableStateOf(false) }
    var selectedAgent by remember { mutableStateOf<AgentSummary?>(null) }
    var conversation by remember { mutableStateOf<HolonConversationSnapshot?>(null) }
    var showAllConversationTurns by remember { mutableStateOf(false) }
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
                setOf("10.0.2.2", "127.0.0.1", "localhost")
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
                        insecureHttpHosts =
                            if (BuildConfig.DEBUG) {
                                setOf("10.0.2.2", "127.0.0.1", "localhost")
                            } else {
                                emptySet()
                            },
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
                        showConnectionEditor = false
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
        if (selectedAgent?.id != agent.id || reset) {
            showAllConversationTurns = false
        }
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

    Box(
        modifier =
            Modifier
                .fillMaxSize()
                .background(HolonPage),
    ) {
        Column(
            modifier =
                Modifier
                    .fillMaxSize()
                    .statusBarsPadding()
                    .navigationBarsPadding()
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp, vertical = 14.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            HolonHeader(state = state)

            HolonSection(
                title = "连接",
                eyebrow = "DEVICE  /  HOLON RUNTIME",
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    StatusPill(
                        label =
                            when (state) {
                                ConnectionState.Connected -> "已连接"
                                ConnectionState.Connecting -> "连接中"
                                ConnectionState.Failed -> "连接失败"
                                ConnectionState.Disconnected -> "未连接"
                            },
                        tone = connectionTone(state),
                    )
                    StatusPill(
                        label = if (networkAvailable) "网络在线" else "网络离线",
                        tone = if (networkAvailable) StatusTone.Success else StatusTone.Danger,
                    )
                }
                Text(
                    text = status,
                    color = if (state == ConnectionState.Failed) HolonDanger else HolonMuted,
                    style = MaterialTheme.typography.bodyMedium,
                )
                if (state == ConnectionState.Connected && !showConnectionEditor) {
                    DetailRow(label = "API", body = baseUrl)
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        OutlinedButton(
                            onClick = { showConnectionEditor = true },
                            shape = RoundedCornerShape(9.dp),
                        ) {
                            Text("编辑连接")
                        }
                        TextButton(onClick = ::logout, enabled = !logoutInProgress) {
                            Text(if (logoutInProgress) "清除中…" else "清除会话", color = HolonMuted)
                        }
                    }
                } else {
                    HolonTextField(
                        value = baseUrl,
                        onValueChange = {
                            baseUrl = it
                            preferences.edit().putString(BASE_URL_KEY, it).apply()
                        },
                        label = "API 地址",
                        singleLine = true,
                    )
                    if (BuildConfig.DEBUG) {
                        HolonTextField(
                            value = sessionCredential,
                            onValueChange = {
                                sessionCredential = it
                                sessionStore.write(it)
                            },
                            label = "Debug session credential",
                            singleLine = true,
                        )
                        Text(
                            "仅用于开发调试：填入已兑换的 session credential。",
                            color = HolonFaint,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Button(
                            onClick = ::connect,
                            enabled = networkAvailable && state != ConnectionState.Connecting && !logoutInProgress,
                            shape = RoundedCornerShape(9.dp),
                            colors = ButtonDefaults.buttonColors(containerColor = HolonAccent),
                        ) {
                            Text(
                                when (state) {
                                    ConnectionState.Connecting -> "连接中…"
                                    ConnectionState.Failed -> "重试连接"
                                    ConnectionState.Connected -> "保存并重连"
                                    ConnectionState.Disconnected -> "连接"
                                },
                            )
                        }
                        OutlinedButton(
                            onClick = {
                                if (state == ConnectionState.Connected) {
                                    showConnectionEditor = false
                                } else {
                                    logout()
                                }
                            },
                            enabled = !logoutInProgress,
                            shape = RoundedCornerShape(9.dp),
                            colors = ButtonDefaults.outlinedButtonColors(contentColor = HolonMuted),
                        ) {
                            Text(if (state == ConnectionState.Connected) "取消" else "清除会话")
                        }
                    }
                }
            }

            HolonSection(
                title = "Agents",
                eyebrow = if (state == ConnectionState.Connected) "${agents.size} REGISTERED" else "ROSTER",
            ) {
                when (state) {
                    ConnectionState.Disconnected -> EmptyHint("连接 Holon runtime 后，这里会显示可用 Agent。")
                    ConnectionState.Connecting -> EmptyHint("正在同步 Agent roster…")
                    ConnectionState.Failed -> EmptyHint("暂时无法读取 Agent，请先修复上方连接。")
                    ConnectionState.Connected -> if (agents.isEmpty()) {
                        EmptyHint("当前 runtime 还没有注册 Agent。")
                    } else if (selectedAgent != null) {
                        AgentRow(
                            agent = selectedAgent!!,
                            selected = true,
                            onClick = { },
                        )
                        TextButton(
                            onClick = {
                                conversationJob?.cancel()
                                selectedAgent = null
                                conversation = null
                                conversationCursor = null
                                toolExecutions = emptyList()
                                clearTaskDetails(cancelSubmit = true)
                            },
                        ) {
                            Text("切换 Agent  ·  ${agents.size} 个可用", color = HolonAccent)
                        }
                    } else {
                        agents.forEach { agent ->
                            AgentRow(
                                agent = agent,
                                selected = selectedAgent?.id == agent.id,
                                onClick = { loadConversation(agent) },
                            )
                        }
                    }
                }
            }

            selectedAgent?.let { agent ->
                HolonSection(
                    title = agent.displayName,
                    eyebrow = "CONVERSATION",
                ) {
                    Text(
                        conversationStatus,
                        color = if (conversationResetRequired) HolonDanger else HolonMuted,
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        OutlinedButton(
                            onClick = { loadConversation(agent, reset = true) },
                            shape = RoundedCornerShape(9.dp),
                        ) {
                            Text(if (conversationResetRequired) "重新同步" else "刷新时间线")
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
                            Text("重置 checkpoint", color = HolonMuted)
                        }
                    }
                    val turns = conversation?.turns.orEmpty()
                    if (turns.isEmpty()) {
                        EmptyHint("还没有可显示的会话记录。")
                    } else {
                        val visibleTurns = if (showAllConversationTurns) turns else turns.take(4)
                        visibleTurns.forEachIndexed { index, turn ->
                            TimelineEntry(
                                id = turn.id,
                                summary = turn.summary,
                                isLast = index == visibleTurns.lastIndex,
                            )
                        }
                        if (turns.size > 4) {
                            TextButton(onClick = { showAllConversationTurns = !showAllConversationTurns }) {
                                Text(
                                    if (showAllConversationTurns) {
                                        "收起到最近 4 条"
                                    } else {
                                        "展开全部 ${turns.size} 条"
                                    },
                                    color = HolonAccent,
                                )
                            }
                        }
                    }
                    conversationCursor?.let {
                        Text("checkpoint  $it", color = HolonFaint, style = MaterialTheme.typography.labelSmall)
                    }
                }

                HolonSection(title = "工具与产物", eyebrow = "EXECUTION") {
                    if (toolExecutions.isEmpty()) {
                        EmptyHint("暂无可见工具调用。")
                    } else {
                        toolExecutions.forEach { tool ->
                            DetailRow(
                                label = "${tool.toolName}  ·  ${tool.status}",
                                body = tool.summary,
                            )
                            if (tool.artifactCount > 0) {
                                Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                                    repeat(tool.artifactCount) { index ->
                                        TextButton(onClick = { loadArtifact(agent, tool, index) }) {
                                            Text("产物 ${index + 1}", color = HolonAccent)
                                        }
                                    }
                                }
                            }
                        }
                    }
                    artifactStatus?.let { Text(it, color = HolonMuted) }
                    artifactContent?.let {
                        Text(
                            it,
                            modifier =
                                Modifier
                                    .fillMaxWidth()
                                    .background(HolonSidebar, RoundedCornerShape(8.dp))
                                    .padding(12.dp),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }

                HolonSection(title = "交给 Agent", eyebrow = "TEXT TASK") {
                    Text(
                        "当前 Android 客户端只提交文字任务；图片入口会在服务端契约就绪后开放。",
                        color = HolonMuted,
                        style = MaterialTheme.typography.bodySmall,
                    )
                    HolonTextField(
                        value = taskInput,
                        onValueChange = { taskInput = it },
                        label = "描述要完成的工作",
                        enabled = taskSubmitJob?.isActive != true && taskRefreshJob?.isActive != true,
                        minLines = 3,
                    )
                    Button(
                        onClick = { sendTask(agent) },
                        enabled =
                            taskInput.isNotBlank() &&
                                taskSubmitJob?.isActive != true &&
                                taskRefreshJob?.isActive != true,
                        shape = RoundedCornerShape(9.dp),
                        colors = ButtonDefaults.buttonColors(containerColor = HolonAccent),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            if (taskSubmitJob?.isActive == true || taskRefreshJob?.isActive == true) {
                                "处理中…"
                            } else {
                                "发送任务"
                            },
                        )
                    }
                    Text(taskStatus, color = HolonMuted, style = MaterialTheme.typography.bodySmall)
                    tasks.forEach { task ->
                        DetailRow(
                            label = "${task.taskId}  ·  ${task.status}",
                            body = task.summary,
                            accent = !isTerminalTaskStatus(task.status),
                        )
                    }
                    taskOutput?.let { output ->
                        DetailRow(
                            label = "最近结果  ·  ${output.status}",
                            body = listOfNotNull(output.resultSummary, output.outputPreview).joinToString("\n").ifBlank { null },
                        )
                    }
                }

                HolonSection(title = "WorkItems", eyebrow = "READ ONLY") {
                    if (workItems.isEmpty()) {
                        EmptyHint("暂无可见 WorkItem。")
                    } else {
                        workItems.forEach { item ->
                            DetailRow(
                                label = "${item.workItemId}  ·  ${item.state}",
                                body = item.objective,
                                accent = !isTerminalTaskStatus(item.state),
                            )
                        }
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
        }
    }
}

@Composable
private fun HolonHeader(state: ConnectionState) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        HolonMark()
        Spacer(Modifier.width(10.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                "Holon",
                color = HolonText,
                style = MaterialTheme.typography.headlineLarge,
            )
            Text(
                "持续运行的 Agent 工作台",
                color = HolonMuted,
                style = MaterialTheme.typography.bodySmall,
            )
        }
        StatusPill(
            label =
                when (state) {
                    ConnectionState.Connected -> "LIVE"
                    ConnectionState.Connecting -> "SYNC"
                    ConnectionState.Failed -> "ERROR"
                    ConnectionState.Disconnected -> "OFFLINE"
                },
            tone = connectionTone(state),
        )
    }
}

private fun connectionTone(state: ConnectionState): StatusTone =
    when (state) {
        ConnectionState.Connected -> StatusTone.Success
        ConnectionState.Connecting -> StatusTone.Accent
        ConnectionState.Failed -> StatusTone.Danger
        ConnectionState.Disconnected -> StatusTone.Neutral
    }

@Composable
private fun HolonTextField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    singleLine: Boolean = false,
    minLines: Int = 1,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        enabled = enabled,
        singleLine = singleLine,
        minLines = minLines,
        shape = RoundedCornerShape(9.dp),
        colors =
            OutlinedTextFieldDefaults.colors(
                focusedBorderColor = HolonAccent,
                unfocusedBorderColor = HolonLineStrong,
                focusedLabelColor = HolonAccent,
                unfocusedLabelColor = HolonMuted,
                focusedContainerColor = Color.White,
                unfocusedContainerColor = Color.White,
            ),
        modifier = modifier.fillMaxWidth(),
    )
}

@Composable
private fun AgentRow(
    agent: AgentSummary,
    selected: Boolean,
    onClick: () -> Unit,
) {
    val active = agent.runtimeStatus.lowercase() in setOf("active", "running", "busy", "working")
    val railColor = if (active) HolonAccent else HolonLineStrong
    Surface(
        modifier =
            Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(10.dp))
                .clickable(onClick = onClick),
        color = if (selected) HolonAccentSoft.copy(alpha = 0.55f) else HolonSidebar,
        shape = RoundedCornerShape(10.dp),
        border = androidx.compose.foundation.BorderStroke(
            1.dp,
            if (selected) HolonAccent.copy(alpha = 0.38f) else HolonLine,
        ),
    ) {
        Row(
            modifier = Modifier.padding(end = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                Modifier
                    .width(4.dp)
                    .height(72.dp)
                    .background(railColor),
            )
            Column(
                modifier =
                    Modifier
                        .weight(1f)
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                verticalArrangement = Arrangement.spacedBy(3.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        agent.displayName,
                        color = HolonText,
                        style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.weight(1f),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (agent.isDefault) {
                        Text("DEFAULT", color = HolonAccent, style = MaterialTheme.typography.labelSmall)
                    }
                }
                Text(
                    agent.id,
                    color = HolonFaint,
                    style = MaterialTheme.typography.labelSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    "${agent.runtimeStatus}  ·  pending ${agent.pending}",
                    color = if (active) HolonAccent else HolonMuted,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Text(if (selected) "已打开" else "打开  →", color = HolonAccent, style = MaterialTheme.typography.labelMedium)
        }
    }
}

@Composable
private fun TimelineEntry(
    id: String,
    summary: String,
    isLast: Boolean,
) {
    Row(modifier = Modifier.fillMaxWidth()) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Box(
                Modifier
                    .padding(top = 5.dp)
                    .size(8.dp)
                    .background(HolonAccent, CircleShape),
            )
            if (!isLast) {
                Box(
                    Modifier
                        .width(1.dp)
                        .height(132.dp)
                        .background(HolonLineStrong),
                )
            }
        }
        Column(
            modifier = Modifier.padding(start = 11.dp, bottom = if (isLast) 0.dp else 12.dp),
            verticalArrangement = Arrangement.spacedBy(3.dp),
        ) {
            Text(id, color = HolonFaint, style = MaterialTheme.typography.labelSmall)
            Text(
                timelinePreview(summary),
                color = HolonText,
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 6,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

private val timelinePreviewKeys = listOf("summary", "preview", "text", "objective", "message")

private fun timelinePreview(raw: String): String {
    val parsed = runCatching { Json.parseToJsonElement(raw) }.getOrNull() ?: return raw
    return findTimelinePreview(parsed) ?: raw
}

private fun findTimelinePreview(element: JsonElement): String? =
    when (element) {
        is JsonObject -> {
            timelinePreviewKeys.forEach { key ->
                val candidate =
                    (element[key] as? JsonPrimitive)
                        ?.contentOrNull
                        ?.takeIf { it.isNotBlank() }
                        ?: return@forEach
                val nested = runCatching { Json.parseToJsonElement(candidate) }.getOrNull()
                return nested?.let(::findTimelinePreview) ?: candidate
            }
            element.values.firstNotNullOfOrNull(::findTimelinePreview)
        }
        is JsonArray -> element.firstNotNullOfOrNull(::findTimelinePreview)
        else -> null
    }

@Composable
private fun DetailRow(
    label: String,
    body: String?,
    accent: Boolean = false,
) {
    Column(
        modifier =
            Modifier
                .fillMaxWidth()
                .border(1.dp, if (accent) HolonAccent.copy(alpha = 0.28f) else HolonLine, RoundedCornerShape(8.dp))
                .background(if (accent) HolonAccentSoft.copy(alpha = 0.34f) else HolonSidebar, RoundedCornerShape(8.dp))
                .padding(horizontal = 12.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            label,
            color = if (accent) HolonAccent else HolonMuted,
            style = MaterialTheme.typography.labelMedium,
        )
        body?.takeIf { it.isNotBlank() }?.let {
            HorizontalDivider(color = HolonLine)
            Text(it, color = HolonText, style = MaterialTheme.typography.bodyMedium)
        }
    }
}
