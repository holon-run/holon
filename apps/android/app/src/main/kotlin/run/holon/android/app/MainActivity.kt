package run.holon.android.app

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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.BearerTokenProvider
import run.holon.android.sdk.CompatibilityResult
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonProtocolException
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.HolonConversationStreamEvent
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

@Composable
private fun HolonApp(context: android.content.Context) {
    val scope = rememberCoroutineScope()
    val sessionStore = remember(context) { createSessionStore(context) }
    var baseUrl by remember { mutableStateOf(defaultBaseUrl()) }
    var sessionCredential by remember {
        mutableStateOf(sessionStore.read().orEmpty())
    }
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
    var conversationJob by remember { mutableStateOf<kotlinx.coroutines.Job?>(null) }
    fun fail(message: String) {
        state = ConnectionState.Failed
        status = message
        agents = emptyList()
        selectedAgent = null
        conversation = null
        conversationJob?.cancel()
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

    fun connect() {
        if (logoutInProgress) return
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
        selectedAgent = null
        conversation = null
        conversationCursor = null
        conversationResetRequired = false
        conversationJob?.cancel()
        val url = baseUrl
        scope.launch {
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
                        client().conversationSnapshot(agent.id, limit = 30)
                    }
                }.onSuccess { snapshot ->
                    if (generation != requestGeneration) return@onSuccess
                    conversation = snapshot
                    conversationCursor = snapshot.snapshotCursor
                    conversationResetRequired = false
                    conversationStatus = "已加载 ${snapshot.turns.size} 个单元，正在接收增量…"
                    conversationJob =
                        scope.launch {
                            runCatching {
                                withContext(Dispatchers.IO) {
                                    client().reconnectingConversationChanges(
                                        agentId = agent.id,
                                        after = snapshot.snapshotCursor,
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
        conversationJob?.cancel()
        sessionCredential = ""
        scope.launch {
            runCatching { withContext(Dispatchers.IO) { client().logout() } }
            if (logoutGeneration == requestGeneration) {
                sessionStore.clear()
                logoutInProgress = false
            }
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
        OutlinedTextField(
            value = baseUrl,
            onValueChange = { baseUrl = it },
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
                enabled = state != ConnectionState.Connecting && !logoutInProgress,
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
        }
    }
}
