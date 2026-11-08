package run.holon.android.app

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.SerialName
import okhttp3.Call
import okhttp3.Request
import java.util.concurrent.TimeUnit

internal const val GITHUB_RELEASES_API =
    "https://api.github.com/repos/holon-run/holon/releases?per_page=20"
private const val ANDROID_PACKAGE = "run.holon.android"
private const val RELEASE_ASSET_PREFIX = "holon-android-v"
private const val RELEASE_ASSET_SUFFIX = ".json"
private val STABLE_TAG = Regex("""^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$""")

@Serializable
private data class ReleaseMetadata(
    val version: String? = null,
    val version_code: Long? = null,
    @SerialName("package")
    val packageName: String? = null,
    val apk: String? = null,
    val apk_sha256: String? = null,
    val apk_size: Long? = null,
)

internal class ReleaseUpdateChecker(
    private val callFactory: Call.Factory = defaultReleaseCallFactory(),
    private val releasesUrl: String = GITHUB_RELEASES_API,
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    suspend fun check(currentVersionCode: Long): ReleaseUpdateResult =
        kotlinx.coroutines.withTimeoutOrNull(60_000) { checkBounded(currentVersionCode) }
            ?: ReleaseUpdateResult.Unavailable("The GitHub update check timed out")

    private suspend fun checkBounded(currentVersionCode: Long): ReleaseUpdateResult =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            val request = Request.Builder()
                .url(releasesUrl)
                .header("Accept", "application/vnd.github+json")
                .header("User-Agent", "Holon-Android/$currentVersionCode")
                .build()
            try {
                val body = callFactory.newCall(request).readCancellable { response, checkActive ->
                    if (!response.isSuccessful) {
                        throw java.io.IOException("GitHub returned HTTP ${response.code}")
                    }
                    response.readBoundedText(2 * 1024 * 1024, checkActive)
                }
                val metadata = mutableMapOf<String, String>()
                for (candidate in parseCandidates(body)) {
                    if (candidate is ParsedRelease.NeedsMetadata) {
                        fetchMetadata(candidate.metadataUrl)?.let { metadata[it.first] = it.second }
                    }
                }
                parse(body, currentVersionCode, metadata)
            } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (_: java.io.IOException) {
                ReleaseUpdateResult.Unavailable("Unable to reach GitHub")
            } catch (_: RuntimeException) {
                ReleaseUpdateResult.Unavailable("Unable to read the GitHub release list")
            }
        }

    internal fun parse(
        payload: String,
        currentVersionCode: Long,
        metadataByUrl: Map<String, String> = emptyMap(),
    ): ReleaseUpdateResult {
        val releases = try {
            json.parseToJsonElement(payload).jsonArray
        } catch (_: RuntimeException) {
            return ReleaseUpdateResult.Unavailable("GitHub returned invalid JSON")
        }
        return try {
            selectCandidate(
                releases.take(20).map { element -> parseRelease(element.jsonObject) },
                currentVersionCode,
                metadataByUrl,
            )
        } catch (_: RuntimeException) {
            ReleaseUpdateResult.InvalidRelease("GitHub returned malformed release metadata")
        }
    }

    private fun parseCandidates(payload: String): List<ParsedRelease> =
        try {
            json.parseToJsonElement(payload).jsonArray.take(20).map { element ->
                parseRelease(element.jsonObject)
            }
        } catch (_: RuntimeException) {
            emptyList()
        }

    private fun selectCandidate(
        parsedReleases: List<ParsedRelease>,
        currentVersionCode: Long,
        metadataByUrl: Map<String, String>,
    ): ReleaseUpdateResult {
        var invalidStableRelease = false
        var invalidReason = "No usable Android release metadata was found"
        val candidates = parsedReleases.mapNotNull { parsed ->
            val resolved = when (parsed) {
                is ParsedRelease.NeedsMetadata ->
                    metadataByUrl[parsed.metadataUrl]?.let { metadata -> toValid(parsed, metadata) }
                        ?: ParsedRelease.Invalid("Android metadata could not be downloaded")
                else -> parsed
            }
            when (resolved) {
                is ParsedRelease.Valid -> resolved.release
                is ParsedRelease.Invalid -> {
                    invalidStableRelease = true
                    invalidReason = resolved.reason
                    null
                }
                ParsedRelease.Irrelevant, is ParsedRelease.NeedsMetadata -> null
            }
        }
        val latest = candidates.maxByOrNull { it.versionCode }
        if (latest != null && latest.versionCode > currentVersionCode) {
            return ReleaseUpdateResult.Available(latest)
        }
        return if (invalidStableRelease && candidates.isEmpty()) {
            ReleaseUpdateResult.InvalidRelease(invalidReason)
        } else {
            ReleaseUpdateResult.NoUpdate
        }
    }

    private suspend fun fetchMetadata(url: String): Pair<String, String>? {
        if (!isSecureUpdateUrl(url)) return null
        val request = Request.Builder()
            .url(url)
            .header("Accept", "application/json")
            .header("User-Agent", "Holon-Android")
            .build()
        return try {
            callFactory.newCall(request).readCancellable { response, checkActive ->
                if (!response.isSuccessful) throw java.io.IOException("Metadata request failed")
                url to response.readBoundedText(64 * 1024, checkActive)
            }
        } catch (cancelled: kotlinx.coroutines.CancellationException) {
            throw cancelled
        } catch (_: java.io.IOException) {
            null
        } catch (_: RuntimeException) {
            null
        }
    }

    private fun parseRelease(release: JsonObject): ParsedRelease {
        if (release["draft"]?.jsonPrimitive?.contentOrNull == "true" ||
            release["prerelease"]?.jsonPrimitive?.contentOrNull == "true"
        ) {
            return ParsedRelease.Irrelevant
        }
        val tag = release["tag_name"]?.jsonPrimitive?.contentOrNull ?: return ParsedRelease.Irrelevant
        val match = STABLE_TAG.matchEntire(tag) ?: return ParsedRelease.Irrelevant
        val version = tag.removePrefix("v")
        val versionCode = versionCode(match)
            ?: return ParsedRelease.Invalid("Stable release $tag has an invalid version code")
        val releaseUrl = release["html_url"]?.jsonPrimitive?.contentOrNull
            ?: return ParsedRelease.Invalid("Stable release $tag has no release URL")
        val body = release["body"]?.jsonPrimitive?.contentOrNull.orEmpty()
        val assets = release["assets"]?.jsonArray
            ?: return ParsedRelease.Invalid("Stable release $tag has no assets")
        val metadataName = "$RELEASE_ASSET_PREFIX$version$RELEASE_ASSET_SUFFIX"
        val metadataAsset = assets.firstOrNull { it.jsonObject["name"]?.jsonPrimitive?.contentOrNull == metadataName }
            ?: return ParsedRelease.Invalid("Stable release $tag has no Android metadata asset")
        val apkName = "$RELEASE_ASSET_PREFIX$version.apk"
        val apkAsset = assets.firstOrNull { it.jsonObject["name"]?.jsonPrimitive?.contentOrNull == apkName }
            ?: return ParsedRelease.Invalid("Stable release $tag has no matching Android APK")
        val metadataUrl = metadataAsset.jsonObject["browser_download_url"]?.jsonPrimitive?.contentOrNull
            ?: return ParsedRelease.Invalid("Stable release $tag has no metadata download URL")
        return ParsedRelease.NeedsMetadata(
            version = version,
            versionCode = versionCode,
            releaseUrl = releaseUrl,
            releaseNotes = body,
            apkName = apkName,
            apkUrl = apkAsset.jsonObject["browser_download_url"]?.jsonPrimitive?.contentOrNull.orEmpty(),
            metadataUrl = metadataUrl,
        )
    }

    private fun versionCode(match: MatchResult): Long? {
        val major = match.groupValues[1].toLongOrNull() ?: return null
        val minor = match.groupValues[2].toLongOrNull() ?: return null
        val patch = match.groupValues[3].toLongOrNull() ?: return null
        if (major > 2_100 || minor > 999 || patch > 999) return null
        return (major * 1_000_000 + minor * 1_000 + patch).takeIf { it in 1..2_100_000_000L }
    }

    private sealed interface ParsedRelease {
        data class NeedsMetadata(
            val version: String,
            val versionCode: Long,
            val releaseUrl: String,
            val releaseNotes: String,
            val apkName: String,
            val apkUrl: String,
            val metadataUrl: String,
        ) : ParsedRelease

        data class Valid(val release: AndroidRelease) : ParsedRelease
        data class Invalid(val reason: String) : ParsedRelease
        data object Irrelevant : ParsedRelease
    }

    private fun toValid(parsed: ParsedRelease.NeedsMetadata, metadataPayload: String): ParsedRelease {
        val metadata = try {
            json.decodeFromString<ReleaseMetadata>(metadataPayload)
        } catch (_: RuntimeException) {
            return ParsedRelease.Invalid("Android metadata is invalid")
        }
        if (metadata.version != parsed.version ||
            metadata.version_code != parsed.versionCode ||
            (metadata.packageName ?: metadata.apk?.substringBeforeLast('/')) != ANDROID_PACKAGE ||
            metadata.apk != parsed.apkName
        ) {
            return ParsedRelease.Invalid("Android metadata does not match release ${parsed.version}")
        }
        return ParsedRelease.Valid(
            AndroidRelease(
                parsed.version,
                parsed.versionCode,
                parsed.releaseUrl,
                parsed.releaseNotes,
                parsed.apkName,
                apkUrl = parsed.apkUrl,
                apkSha256 = metadata.apk_sha256.orEmpty(),
                apkSize = metadata.apk_size ?: 0,
            ),
        )
    }
}

private fun defaultReleaseCallFactory(): Call.Factory =
    okhttp3.OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(15, TimeUnit.SECONDS)
        .callTimeout(20, TimeUnit.SECONDS)
        .followSslRedirects(false)
        .build()
