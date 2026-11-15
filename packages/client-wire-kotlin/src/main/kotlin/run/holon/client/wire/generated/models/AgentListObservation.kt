// Generated from docs/website/reference/openapi.json by web-gui/openapi-tools.
// Do not edit by hand. Run `make transport-types` from the repository root.

package run.holon.client.wire.generated.models

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class AgentListObservation(
    @SerialName("identity_revision")
    val identityRevision: kotlin.ULong,

    @SerialName("owner_epoch")
    val ownerEpoch: kotlin.ULong,

    @SerialName("publication_generation")
    val publicationGeneration: kotlin.ULong,

    @SerialName("status")
    val status: AgentListObservationStatus,
)
