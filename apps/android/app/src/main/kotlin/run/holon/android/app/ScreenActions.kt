package run.holon.android.app

import android.net.Uri
import run.holon.android.sdk.AgentSummary
import run.holon.android.sdk.HolonAgentEvent
import run.holon.android.sdk.HolonModelCatalog
import run.holon.android.sdk.HolonBrief
import run.holon.android.sdk.HolonBriefReadState
import run.holon.android.sdk.HolonConversationActivity
import run.holon.android.sdk.HolonConversationDetail
import run.holon.android.sdk.HolonConversationSnapshot
import run.holon.android.sdk.SseReconnectPolicy
import run.holon.android.sdk.HolonConversationStreamEvent
import run.holon.android.sdk.HolonConversationTurn
import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonFileReferenceResult
import run.holon.android.sdk.HolonRosterSnapshot
import run.holon.android.sdk.HolonSseConnection
import run.holon.android.sdk.HolonToolExecutionSnapshot
import run.holon.android.sdk.HolonWorkItemSnapshot
import run.holon.android.sdk.HolonTaskSnapshot
import run.holon.android.sdk.HolonTaskOutputSnapshot
import run.holon.android.sdk.HolonWorkspace
import run.holon.android.sdk.HolonWorkspaceDirectory
import run.holon.android.sdk.toConversationEvent

internal interface ConnectionActions {
    fun applyScannedAddress(value: String): Unit
    fun cancelAddNetwork(): Unit
    fun cancelPairing(): Unit
    fun clearError(): Unit
    fun confirmPairing(): Unit
    fun login(): Unit
    fun reportScanFailure(): Unit
    fun setAllowInsecureHttp(value: Boolean): Unit
    fun setBaseUrl(value: String): Unit
    fun setToken(value: String): Unit
    fun startOidcLogin(): String?
    fun switchNetwork(networkId: String): Unit
    fun toggleToken(): Unit
}

internal interface AgentsActions {
    fun beginAddNetwork(): Unit
    fun openAgent(agent: AgentSummary): Unit
    fun selectMainDestination(destination: MainDestination): Unit
    fun setSearch(value: String): Unit
    fun switchNetwork(networkId: String): Unit
}

internal interface SettingsActions {
    val traceRecorder: TraceRecorder
    fun beginAddNetwork(): Unit
    fun logout(): Unit
    fun refresh(showProgress: Boolean = true): Unit
    fun relogin(): Unit
    fun shareTraceWithAgent(): Unit
    fun switchNetwork(networkId: String): Unit
}

internal interface ConversationActions : WorkActions, FilesActions {
    fun addAttachment(uri: Uri, preferredKind: String? = null): Unit
    fun clearAgentModel(): Unit
    fun clearError(): Unit
    fun clearPreparedArtifact(): Unit
    fun closeActivity(): Unit
    fun closeBrief(): Unit
    fun closePlanFile(): Unit
    fun closeTurn(): Unit
    fun editFailedMessage(message: OutboxEntity): Unit
    fun ensureBriefs(ids: List<String>, retry: Boolean = false): Unit
    fun handleSystemBack(): Boolean
    fun inspectActivity(activity: HolonConversationActivity): Unit
    fun loadModelCatalog(refresh: Boolean = false): Unit
    fun loadOlderActivities(): Unit
    fun loadOlderTurns(): Unit
    fun markBriefRead(agentId: String, readThroughEventSeq: Long): Unit
    fun openAgent(agent: AgentSummary): Unit
    fun openMessageFile(reference: MessageFileReference): Unit
    fun openRelatedWorkItem(workItemId: String): Unit
    fun openTurn(turn: HolonConversationTurn): Unit
    fun removeAttachment(index: Int): Unit
    fun removeFailedMessage(message: OutboxEntity): Unit
    fun retryMessage(message: OutboxEntity): Unit
    fun send(): Unit
    fun setAgentModel(model: String, reasoningEffort: String?): Unit
    fun setTurnFullScreen(fullScreen: Boolean): Unit
    fun stopCurrentTurn(): Unit
    fun updateDraft(value: String): Unit
}

internal interface WorkActions {
    fun closeTask(): Unit
    fun closeWorkItem(): Unit
    fun loadMoreWorkItems(): Unit
    fun loadTaskOutput(): Unit
    fun openBrief(briefId: String): Unit
    fun openTask(task: HolonTaskSnapshot): Unit
    fun openWorkItem(item: HolonWorkItemSnapshot): Unit
    fun openWorkItemPlan(): Unit
    fun refreshTasks(): Unit
    fun selectAgentSection(section: AgentSection): Unit
}

internal interface FilesActions {
    fun navigateWorkspaceTo(path: String): Unit
    fun openWorkspaceEntry(name: String, directory: Boolean): Unit
    fun prepareArtifact(locator: String, name: String): Unit
    fun returnFromMessageFile(): Unit
    fun saveArtifactToDevice(artifact: PreparedArtifact, destination: Uri): Unit
    fun selectWorkspace(workspace: HolonWorkspace): Unit
}

internal interface ShareActions {
    fun dismissShare(): Unit
    fun sendShare(agent: AgentSummary): Unit
    fun updateShareText(text: String): Unit
}

/** UI depends on page actions, not lifecycle, AndroidViewModel, or transport. */
internal class AndroidScreenActions(private val delegate: HolonViewModel) {
    val connection: ConnectionActions = object : ConnectionActions {
        override fun applyScannedAddress(value: String): Unit { delegate.applyScannedAddress(value) }
        override fun cancelAddNetwork(): Unit { delegate.cancelAddNetwork() }
        override fun cancelPairing(): Unit { delegate.cancelPairing() }
        override fun clearError(): Unit { delegate.clearError() }
        override fun confirmPairing(): Unit { delegate.confirmPairing() }
        override fun login(): Unit { delegate.login() }
        override fun reportScanFailure(): Unit { delegate.reportScanFailure() }
        override fun setAllowInsecureHttp(value: Boolean): Unit { delegate.setAllowInsecureHttp(value) }
        override fun setBaseUrl(value: String): Unit { delegate.setBaseUrl(value) }
        override fun setToken(value: String): Unit { delegate.setToken(value) }
        override fun startOidcLogin(): String? { return delegate.startOidcLogin() }
        override fun switchNetwork(networkId: String): Unit { delegate.switchNetwork(networkId) }
        override fun toggleToken(): Unit { delegate.toggleToken() }
    }
    val agents: AgentsActions = object : AgentsActions {
        override fun beginAddNetwork(): Unit { delegate.beginAddNetwork() }
        override fun openAgent(agent: AgentSummary): Unit { delegate.openAgent(agent) }
        override fun selectMainDestination(destination: MainDestination): Unit { delegate.selectMainDestination(destination) }
        override fun setSearch(value: String): Unit { delegate.setSearch(value) }
        override fun switchNetwork(networkId: String): Unit { delegate.switchNetwork(networkId) }
    }
    val settings: SettingsActions = object : SettingsActions {
        override val traceRecorder get() = delegate.traceRecorder
        override fun beginAddNetwork(): Unit { delegate.beginAddNetwork() }
        override fun logout(): Unit { delegate.logout() }
        override fun refresh(showProgress: Boolean): Unit { delegate.refresh(showProgress) }
        override fun relogin(): Unit { delegate.relogin() }
        override fun shareTraceWithAgent(): Unit { delegate.shareTraceWithAgent() }
        override fun switchNetwork(networkId: String): Unit { delegate.switchNetwork(networkId) }
    }
    val conversation: ConversationActions = object : ConversationActions {
        override fun addAttachment(uri: Uri, preferredKind: String?): Unit { delegate.addAttachment(uri, preferredKind) }
        override fun clearAgentModel(): Unit { delegate.clearAgentModel() }
        override fun clearError(): Unit { delegate.clearError() }
        override fun clearPreparedArtifact(): Unit { delegate.clearPreparedArtifact() }
        override fun closeActivity(): Unit { delegate.closeActivity() }
        override fun closeBrief(): Unit { delegate.closeBrief() }
        override fun closePlanFile(): Unit { delegate.closePlanFile() }
        override fun closeTask(): Unit { delegate.closeTask() }
        override fun closeTurn(): Unit { delegate.closeTurn() }
        override fun closeWorkItem(): Unit { delegate.closeWorkItem() }
        override fun editFailedMessage(message: OutboxEntity): Unit { delegate.editFailedMessage(message) }
        override fun ensureBriefs(ids: List<String>, retry: Boolean): Unit { delegate.ensureBriefs(ids, retry) }
        override fun handleSystemBack(): Boolean { return delegate.handleSystemBack() }
        override fun inspectActivity(activity: HolonConversationActivity): Unit { delegate.inspectActivity(activity) }
        override fun loadModelCatalog(refresh: Boolean): Unit { delegate.loadModelCatalog(refresh) }
        override fun loadMoreWorkItems(): Unit { delegate.loadMoreWorkItems() }
        override fun loadOlderActivities(): Unit { delegate.loadOlderActivities() }
        override fun loadOlderTurns(): Unit { delegate.loadOlderTurns() }
        override fun loadTaskOutput(): Unit { delegate.loadTaskOutput() }
        override fun markBriefRead(agentId: String, readThroughEventSeq: Long): Unit { delegate.markBriefRead(agentId, readThroughEventSeq) }
        override fun navigateWorkspaceTo(path: String): Unit { delegate.navigateWorkspaceTo(path) }
        override fun openAgent(agent: AgentSummary): Unit { delegate.openAgent(agent) }
        override fun openBrief(briefId: String): Unit { delegate.openBrief(briefId) }
        override fun openMessageFile(reference: MessageFileReference): Unit { delegate.openMessageFile(reference) }
        override fun openRelatedWorkItem(workItemId: String): Unit { delegate.openRelatedWorkItem(workItemId) }
        override fun openTask(task: HolonTaskSnapshot): Unit { delegate.openTask(task) }
        override fun openTurn(turn: HolonConversationTurn): Unit { delegate.openTurn(turn) }
        override fun openWorkItem(item: HolonWorkItemSnapshot): Unit { delegate.openWorkItem(item) }
        override fun openWorkItemPlan(): Unit { delegate.openWorkItemPlan() }
        override fun openWorkspaceEntry(name: String, directory: Boolean): Unit { delegate.openWorkspaceEntry(name, directory) }
        override fun prepareArtifact(locator: String, name: String): Unit { delegate.prepareArtifact(locator, name) }
        override fun refreshTasks(): Unit { delegate.refreshTasks() }
        override fun removeAttachment(index: Int): Unit { delegate.removeAttachment(index) }
        override fun removeFailedMessage(message: OutboxEntity): Unit { delegate.removeFailedMessage(message) }
        override fun retryMessage(message: OutboxEntity): Unit { delegate.retryMessage(message) }
        override fun returnFromMessageFile(): Unit { delegate.returnFromMessageFile() }
        override fun saveArtifactToDevice(artifact: PreparedArtifact, destination: Uri): Unit { delegate.saveArtifactToDevice(artifact, destination) }
        override fun selectAgentSection(section: AgentSection): Unit { delegate.selectAgentSection(section) }
        override fun selectWorkspace(workspace: HolonWorkspace): Unit { delegate.selectWorkspace(workspace) }
        override fun send(): Unit { delegate.send() }
        override fun setAgentModel(model: String, reasoningEffort: String?): Unit { delegate.setAgentModel(model, reasoningEffort) }
        override fun setTurnFullScreen(fullScreen: Boolean): Unit { delegate.setTurnFullScreen(fullScreen) }
        override fun stopCurrentTurn(): Unit { delegate.stopCurrentTurn() }
        override fun updateDraft(value: String): Unit { delegate.updateDraft(value) }
    }
    val work: WorkActions = object : WorkActions {
        override fun closeTask(): Unit { delegate.closeTask() }
        override fun closeWorkItem(): Unit { delegate.closeWorkItem() }
        override fun loadMoreWorkItems(): Unit { delegate.loadMoreWorkItems() }
        override fun loadTaskOutput(): Unit { delegate.loadTaskOutput() }
        override fun openBrief(briefId: String): Unit { delegate.openBrief(briefId) }
        override fun openTask(task: HolonTaskSnapshot): Unit { delegate.openTask(task) }
        override fun openWorkItem(item: HolonWorkItemSnapshot): Unit { delegate.openWorkItem(item) }
        override fun openWorkItemPlan(): Unit { delegate.openWorkItemPlan() }
        override fun refreshTasks(): Unit { delegate.refreshTasks() }
        override fun selectAgentSection(section: AgentSection): Unit { delegate.selectAgentSection(section) }
    }
    val files: FilesActions = object : FilesActions {
        override fun navigateWorkspaceTo(path: String): Unit { delegate.navigateWorkspaceTo(path) }
        override fun openWorkspaceEntry(name: String, directory: Boolean): Unit { delegate.openWorkspaceEntry(name, directory) }
        override fun prepareArtifact(locator: String, name: String): Unit { delegate.prepareArtifact(locator, name) }
        override fun returnFromMessageFile(): Unit { delegate.returnFromMessageFile() }
        override fun saveArtifactToDevice(artifact: PreparedArtifact, destination: Uri): Unit { delegate.saveArtifactToDevice(artifact, destination) }
        override fun selectWorkspace(workspace: HolonWorkspace): Unit { delegate.selectWorkspace(workspace) }
    }
    val share: ShareActions = object : ShareActions {
        override fun dismissShare(): Unit { delegate.dismissShare() }
        override fun sendShare(agent: AgentSummary): Unit { delegate.sendShare(agent) }
        override fun updateShareText(text: String): Unit { delegate.updateShareText(text) }
    }
}
