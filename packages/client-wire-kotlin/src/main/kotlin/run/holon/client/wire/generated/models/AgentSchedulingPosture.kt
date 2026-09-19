// Generated from docs/website/reference/openapi.json by web-gui/openapi-tools.
// Do not edit by hand. Run `make transport-types` from the repository root.

package run.holon.client.wire.generated.models

import kotlinx.serialization.Serializable

/**
 * An open wire enum. Known values are exposed as constants, while unknown
 * values remain decodable so newer runtimes stay compatible with this client.
 */
@JvmInline
@Serializable
value class AgentSchedulingPosture(val value: kotlin.String) {
    override fun toString(): kotlin.String = value

    companion object {
        val unknown: AgentSchedulingPosture =
            AgentSchedulingPosture("unknown")

        val stopped: AgentSchedulingPosture =
            AgentSchedulingPosture("stopped")

        val active_turn: AgentSchedulingPosture =
            AgentSchedulingPosture("active_turn")

        val has_queued_input: AgentSchedulingPosture =
            AgentSchedulingPosture("has_queued_input")

        val has_runnable_work: AgentSchedulingPosture =
            AgentSchedulingPosture("has_runnable_work")

        val waiting_for_task: AgentSchedulingPosture =
            AgentSchedulingPosture("waiting_for_task")

        val waiting_for_external: AgentSchedulingPosture =
            AgentSchedulingPosture("waiting_for_external")

        val waiting_for_operator: AgentSchedulingPosture =
            AgentSchedulingPosture("waiting_for_operator")

        val blocked: AgentSchedulingPosture =
            AgentSchedulingPosture("blocked")

        val idle: AgentSchedulingPosture =
            AgentSchedulingPosture("idle")

    }
}
