package run.holon.android.sdk

import kotlinx.serialization.json.Json

/**
 * The single JSON configuration used for Holon wire payloads.
 *
 * Holon responses are forward-compatible and may add fields before the SDK
 * consumes them. Generated transport models therefore must not be decoded with
 * kotlinx.serialization's strict unknown-key default.
 */
public object HolonWire {
    public val json: Json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }
}
