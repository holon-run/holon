@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue

import androidx.compose.material.icons.filled.Settings
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

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
            AppPhase.SignedOut -> LoginScreen(state, viewModel.screenActions.connection)
            AppPhase.AddingNetwork -> LoginScreen(state, viewModel.screenActions.connection, addingNetwork = true)
            AppPhase.Ready -> MainShell(state, viewModel)
        }
    }
    if (state.phase == AppPhase.Ready && state.pendingShare != null) {
        ShareToAgentDialog(state, viewModel.screenActions.share)
    }
}

@Composable
internal fun MainShell(state: HolonUiState, viewModel: HolonViewModel) {
    val savedPages = rememberSaveableStateHolder()
    savedPages.SaveableStateProvider("${state.session?.scopeKey}:${state.selectedAgent?.id ?: state.mainDestination.name}") {
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val useListDetail = maxWidth >= 840.dp && state.mainDestination == MainDestination.Agents
        when {
            useListDetail -> {
                Row(Modifier.fillMaxSize()) {
                    Box(Modifier.width(380.dp).fillMaxHeight()) {
                        AgentsScreen(state, viewModel.screenActions.agents)
                    }
                    Box(Modifier.width(1.dp).fillMaxHeight().background(MaterialTheme.colorScheme.outlineVariant))
                    Box(Modifier.weight(1f).fillMaxHeight()) {
                        if (state.selectedAgent != null) {
                            ConversationScreen(state, viewModel.screenActions.conversation)
                        } else {
                            EmptyPage(ui("选择一个 Agent"), ui("结果、工作和文件将在这里打开。"))
                        }
                    }
                }
            }
            state.selectedAgent != null -> ConversationScreen(state, viewModel.screenActions.conversation)
            state.mainDestination == MainDestination.Settings -> SettingsScreen(
                state = state,
                viewModel = viewModel.screenActions.settings,
                onBack = { viewModel.selectMainDestination(MainDestination.Agents) },
            )
            else -> AgentsScreen(state, viewModel.screenActions.agents)
        }
    }
    }
}

internal enum class AgentFilter(private val sourceLabel: String) {
    All("全部"),
    Attention("需回应"),
    NewResults("新结果"),
    Active("工作中"),
    ;

    val label: String get() = ui(sourceLabel)
}
