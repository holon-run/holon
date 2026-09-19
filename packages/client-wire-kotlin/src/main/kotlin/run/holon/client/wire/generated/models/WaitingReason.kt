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
value class WaitingReason(val value: kotlin.String) {
    override fun toString(): kotlin.String = value

    companion object {
        val awaiting_operator_input: WaitingReason =
            WaitingReason("awaiting_operator_input")

        val awaiting_external_change: WaitingReason =
            WaitingReason("awaiting_external_change")

        val awaiting_task_result: WaitingReason =
            WaitingReason("awaiting_task_result")

        val awaiting_timer: WaitingReason =
            WaitingReason("awaiting_timer")

    }
}
