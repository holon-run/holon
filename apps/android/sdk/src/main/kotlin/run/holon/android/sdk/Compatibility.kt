package run.holon.android.sdk

import run.holon.client.wire.generated.models.HandshakeResponse

public const val HOLON_CONTROL_PROTOCOL_NAME: String = "holon-control"
public const val HOLON_CONTROL_PROTOCOL_VERSION: Int = 1

public data class HolonServerLimits(
    val promptBodyMaxBytes: Long,
    val promptFileAttachmentMaxBytes: Long,
    val promptImageAttachmentMaxBytes: Long,
)

public data class HolonServerInfo(
    val defaultAgentId: String,
    val authMode: String,
    val authRequired: Boolean,
    val capabilities: Set<String>,
    val limits: HolonServerLimits? = null,
)

public sealed interface CompatibilityResult {
    public data class Compatible(
        val server: HolonServerInfo,
    ) : CompatibilityResult

    public data class UnsupportedProtocol(
        val actualName: String,
        val actualVersion: Int,
    ) : CompatibilityResult

    public data class MissingCapabilities(
        val capabilities: Set<String>,
    ) : CompatibilityResult

    public data object RejectedHandshake : CompatibilityResult
}

internal fun HandshakeResponse.checkCompatibility(
    requiredCapabilities: Set<String> = emptySet(),
): CompatibilityResult {
    if (!ok) {
        return CompatibilityResult.RejectedHandshake
    }
    if (
        protocol.name != HOLON_CONTROL_PROTOCOL_NAME ||
        protocol.version != HOLON_CONTROL_PROTOCOL_VERSION
    ) {
        return CompatibilityResult.UnsupportedProtocol(
            actualName = protocol.name,
            actualVersion = protocol.version,
        )
    }

    val missingCapabilities = requiredCapabilities - capabilities.toSet()
    if (missingCapabilities.isNotEmpty()) {
        return CompatibilityResult.MissingCapabilities(missingCapabilities)
    }

    return CompatibilityResult.Compatible(
        HolonServerInfo(
            defaultAgentId = runtime.defaultAgent,
            authMode = auth.mode,
            authRequired = auth.required,
            capabilities = capabilities.toSet(),
            limits = limits?.let {
                HolonServerLimits(
                    promptBodyMaxBytes = it.promptBodyMaxBytes.toLong(),
                    promptFileAttachmentMaxBytes = it.promptFileAttachmentMaxBytes.toLong(),
                    promptImageAttachmentMaxBytes = it.promptImageAttachmentMaxBytes.toLong(),
                )
            },
        ),
    )
}
