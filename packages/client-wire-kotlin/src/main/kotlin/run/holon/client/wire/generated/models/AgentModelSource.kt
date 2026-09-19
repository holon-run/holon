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
value class AgentModelSource(val value: kotlin.String) {
    override fun toString(): kotlin.String = value

    companion object {
        val runtime_default: AgentModelSource =
            AgentModelSource("runtime_default")

        val agent_override: AgentModelSource =
            AgentModelSource("agent_override")

    }
}
