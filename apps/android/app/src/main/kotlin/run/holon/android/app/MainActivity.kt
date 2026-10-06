package run.holon.android.app

import android.content.Intent
import android.os.Bundle
import android.os.SystemClock
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.addCallback
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : ComponentActivity() {
    private val container by lazy { AppContainer(applicationContext) }
    private val viewModel: HolonViewModel by viewModels {
        HolonViewModel.factory(application, container)
    }
    private var lastExitRequestAt = 0L

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        UiCopy.initialize(this)
        enableEdgeToEdge()
        lifecycle.addObserver(
            LifecycleEventObserver { _, event ->
                if (event == Lifecycle.Event.ON_RESUME) viewModel.onForeground()
                if (event == Lifecycle.Event.ON_PAUSE) viewModel.onBackground()
            },
        )
        onBackPressedDispatcher.addCallback(this) {
            if (!viewModel.handleSystemBack()) {
                val now = SystemClock.elapsedRealtime()
                if (now - lastExitRequestAt <= 2_000) {
                    finish()
                } else {
                    lastExitRequestAt = now
                    Toast.makeText(this@MainActivity, ui("再按一次返回桌面"), Toast.LENGTH_SHORT).show()
                }
            }
        }
        setContent {
            HolonTheme {
                HolonApp(viewModel)
            }
        }
        receiveShare(intent)
        lifecycleScope.launch {
            viewModel.state
                .map { state ->
                    val scope = state.session?.scopeKey.takeIf { state.phase == AppPhase.Ready }
                    scope to if (scope == null) emptyList() else
                        (listOfNotNull(state.selectedAgent) + state.recentAgents).distinctBy { it.id }.take(4)
                }
                .distinctUntilChanged { previous, current ->
                    previous.first == current.first &&
                        previous.second.map { it.id to it.displayName } == current.second.map { it.id to it.displayName }
                }
                .collectLatest { (scope, agents) ->
                    withContext(Dispatchers.IO) {
                        runCatching { AgentShareShortcuts.publish(applicationContext, scope, agents) }
                            .onFailure { error ->
                                container.traceRecorder.record(
                                    TraceScope.Global,
                                    TraceLevel.WARN,
                                    "share",
                                    "share.shortcuts.failed",
                                    attributes = mapOf("error" to error::class.simpleName.orEmpty()),
                                )
                            }
                    }
                }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        receiveShare(intent)
    }

    private fun receiveShare(incoming: Intent) {
        if (incoming.action == Intent.ACTION_SEND || incoming.action == Intent.ACTION_SEND_MULTIPLE) {
            val shared = incomingShare(this, incoming)
            // The payload now belongs to the ViewModel. A retained ACTION_SEND would be replayed
            // by onCreate after an onNewIntent delivery or an Activity recreation.
            setIntent(Intent(Intent.ACTION_MAIN).setClass(this, MainActivity::class.java))
            shared?.let(viewModel::offerShare)
            return
        }
        if (incoming.action == Intent.ACTION_VIEW) {
            if (incoming.data?.let(viewModel::handleOidcCallback) == true) return
            val shortcutId = incoming.getStringExtra(AgentShareShortcuts.EXTRA_SHORTCUT_ID) ?: return
            ShortcutManagerCompat.reportShortcutUsed(this, shortcutId)
            lifecycleScope.launch {
                val ready = viewModel.state.first { it.phase == AppPhase.Ready && it.session != null }
                AgentShareShortcuts.target(ready.session!!.scopeKey, ready.agents, shortcutId)?.let(viewModel::openAgent)
            }
        }
    }
}
