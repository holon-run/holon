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
value class AgentStatus(val value: kotlin.String) {
    override fun toString(): kotlin.String = value

    companion object {
        val booting: AgentStatus =
            AgentStatus("booting")

        val awake_idle: AgentStatus =
            AgentStatus("awake_idle")

        val awake_running: AgentStatus =
            AgentStatus("awake_running")

        val awaiting_task: AgentStatus =
            AgentStatus("awaiting_task")

        val asleep: AgentStatus =
            AgentStatus("asleep")

        val stopped: AgentStatus =
            AgentStatus("stopped")

    }
}
