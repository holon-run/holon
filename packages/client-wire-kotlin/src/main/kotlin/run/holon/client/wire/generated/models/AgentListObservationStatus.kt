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
value class AgentListObservationStatus(val value: kotlin.String) {
    override fun toString(): kotlin.String = value

    companion object {
        val current: AgentListObservationStatus =
            AgentListObservationStatus("current")

        val committing: AgentListObservationStatus =
            AgentListObservationStatus("committing")

        val reconciliation_pending: AgentListObservationStatus =
            AgentListObservationStatus("reconciliation_pending")

        val repair_pending: AgentListObservationStatus =
            AgentListObservationStatus("repair_pending")

        val owner_pending: AgentListObservationStatus =
            AgentListObservationStatus("owner_pending")

    }
}
