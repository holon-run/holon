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
import run.holon.android.sdk.BearerTokenProvider
import run.holon.android.sdk.CompatibilityResult
import run.holon.android.sdk.HolonHttpClient
import run.holon.android.sdk.SessionCredentialStore

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
    var agents by remember { mutableStateOf(emptyList<String>()) }
    fun fail(message: String) {
        state = ConnectionState.Failed
        status = "连接失败：$message"
        agents = emptyList()
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
        if (BuildConfig.DEBUG) {
            if (sessionCredential.isBlank()) {
                sessionStore.clear()
            } else {
                sessionStore.write(sessionCredential)
            }
        }
        state = ConnectionState.Connecting
        status = "正在连接…"
        scope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    val compatibility = client().handshake()
                    val agentList = client().listAgents()
                    compatibility to agentList.map { "${it.displayName} (${it.id})" }
                }
            }.onSuccess { (compatibility, agentList) ->
                when (compatibility) {
                    is CompatibilityResult.Compatible -> {
                        state = ConnectionState.Connected
                        status =
                            "已连接 · ${compatibility.server.authMode} · " +
                                "${compatibility.server.capabilities.size} capabilities"
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
                fail(error.message ?: error::class.simpleName.orEmpty())
            }
        }
    }

    fun logout() {
        scope.launch {
            runCatching { withContext(Dispatchers.IO) { client().logout() } }
            sessionCredential = ""
            state = ConnectionState.Disconnected
            status = "已登出"
            agents = emptyList()
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
            Button(onClick = ::connect, enabled = state != ConnectionState.Connecting) {
                Text(if (state == ConnectionState.Connecting) "连接中…" else "连接")
            }
            TextButton(onClick = ::logout) {
                Text("登出")
            }
        }
        Text(status, color = if (state == ConnectionState.Failed) {
            MaterialTheme.colorScheme.error
        } else {
            MaterialTheme.colorScheme.onSurface
        })
        Spacer(Modifier.height(8.dp))
        Text("Agents", style = MaterialTheme.typography.titleLarge)
        if (agents.isEmpty()) {
            Text("暂无 agent")
        } else {
            agents.forEach { Text("• $it") }
        }
    }
}
