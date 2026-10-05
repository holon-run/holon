package run.holon.android.sdk

/** A permission error is not evidence that the login credential is invalid. */
public fun requiresSessionRenewal(statusCode: Int, code: String?): Boolean =
    statusCode == 401 && (code == null || code in setOf(
        "auth_required", "invalid_static_token", "pairing_invalid_or_expired",
        "session_invalid_or_expired", "session_expired_or_revoked", "session_user_disabled",
    ))
