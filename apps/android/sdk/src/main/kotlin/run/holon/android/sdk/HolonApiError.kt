package run.holon.android.sdk

import run.holon.client.wire.generated.models.ErrorResponse

public data class HolonApiError(
    val code: String,
    val message: String,
    val retryable: Boolean,
    val detail: String?,
    val domain: String?,
    val context: Map<String, String>,
)

internal fun ErrorResponse.toHolonApiError(): HolonApiError =
    HolonApiError(
        code = code,
        message = error,
        retryable = retryable ?: false,
        detail = context?.get("detail"),
        domain = domain?.value,
        context = context.orEmpty(),
    )
