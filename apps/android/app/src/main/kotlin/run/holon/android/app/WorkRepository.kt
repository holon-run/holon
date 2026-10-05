package run.holon.android.app

import run.holon.android.sdk.HolonTaskSnapshot
import run.holon.android.sdk.HolonTaskOutputSnapshot
import run.holon.android.sdk.HolonToolExecutionSnapshot
import run.holon.android.sdk.HolonWorkItemSnapshot

/** Work detail never supplies authoritative conversation state. */
internal class WorkRepository(private val sessions: SessionCoordinator) {
    fun items(agentId: String, limit: Int): List<HolonWorkItemSnapshot> =
        sessions.read { it.workItemSnapshots(agentId, limit = limit) }
    fun item(agentId: String, id: String): HolonWorkItemSnapshot =
        sessions.read { it.workItemSnapshot(agentId, id) }
    fun tasks(agentId: String): List<HolonTaskSnapshot> =
        sessions.read { it.taskSnapshots(agentId, limit = 50) }
    fun task(agentId: String, id: String): HolonTaskSnapshot =
        sessions.read { it.taskStatusSnapshot(agentId, id) }
    fun output(agentId: String, id: String): HolonTaskOutputSnapshot =
        sessions.read { it.taskOutputSnapshot(agentId, id, block = false) }
    fun tool(agentId: String, id: String): HolonToolExecutionSnapshot =
        sessions.read { it.toolExecutionSnapshot(agentId, id) }
}

