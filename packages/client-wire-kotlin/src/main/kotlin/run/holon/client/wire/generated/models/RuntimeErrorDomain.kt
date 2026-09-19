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
value class RuntimeErrorDomain(val value: kotlin.String) {
    override fun toString(): kotlin.String = value

    companion object {
        val runtime: RuntimeErrorDomain =
            RuntimeErrorDomain("runtime")

        val storage: RuntimeErrorDomain =
            RuntimeErrorDomain("storage")

        val policy: RuntimeErrorDomain =
            RuntimeErrorDomain("policy")

        val io: RuntimeErrorDomain =
            RuntimeErrorDomain("io")

        val conflict: RuntimeErrorDomain =
            RuntimeErrorDomain("conflict")

        val not_found: RuntimeErrorDomain =
            RuntimeErrorDomain("not_found")

        val validation: RuntimeErrorDomain =
            RuntimeErrorDomain("validation")

        val provider: RuntimeErrorDomain =
            RuntimeErrorDomain("provider")

        val tool: RuntimeErrorDomain =
            RuntimeErrorDomain("tool")

        val task: RuntimeErrorDomain =
            RuntimeErrorDomain("task")

        val http: RuntimeErrorDomain =
            RuntimeErrorDomain("http")

        val unknown: RuntimeErrorDomain =
            RuntimeErrorDomain("unknown")

    }
}
