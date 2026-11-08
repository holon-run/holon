@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package run.holon.android.app

import androidx.compose.runtime.getValue


import androidx.compose.material.icons.filled.Settings
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.width
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
            AppPhase.Ready -> RuntimeBrowserHost(state, viewModel) { MainShell(state, viewModel) }
        }
    }
    if (state.phase == AppPhase.Ready && state.pendingShare != null) {
        ShareToAgentDialog(state, viewModel.screenActions.share)
    }
    ChannelUpdateHost(state.updateState.actionRequest, viewModel::consumeUpdateAction)
    val update = state.updateState.result
    if (state.updateState.showPrompt && update?.hasUpdate == true) {
        AlertDialog(
            onDismissRequest = viewModel::dismissUpdatePrompt,
            title = { Text(ui("发现 Holon 新版本")) },
            text = {
                Text(
                    (update as? ReleaseUpdateResult.Available)?.release?.releaseNotes?.takeIf { it.isNotBlank() }
                        ?: if (update is ReleaseUpdateResult.PlayAvailable) ui("通过 Google Play 下载并安装更新。")
                        else ui("下载后将校验安装包，再由系统确认安装。"),
                )
            },
            confirmButton = {
                TextButton(onClick = viewModel::requestUpdate) { Text(ui("更新")) }
            },
            dismissButton = {
                TextButton(onClick = viewModel::dismissUpdatePrompt) { Text(ui("稍后")) }
            },
        )
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
