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
value class WorkspaceAccessMode(val value: kotlin.String) {
    override fun toString(): kotlin.String = value

    companion object {
        val shared_read: WorkspaceAccessMode =
            WorkspaceAccessMode("shared_read")

        val exclusive_write: WorkspaceAccessMode =
            WorkspaceAccessMode("exclusive_write")

    }
}
