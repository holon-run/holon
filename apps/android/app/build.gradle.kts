plugins {
    id("app.cash.paparazzi")
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.kapt")
    kotlin("plugin.serialization")
}

val appVersion = providers.environmentVariable("HOLON_ANDROID_VERSION_NAME")
    .orElse(provider {
        Regex("""(?m)^version = "([^"]+)"$""")
            .find(rootProject.file("../../Cargo.toml").readText())!!.groupValues[1]
    }).get()
val versionParts = Regex("""(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)""")
    .matchEntire(appVersion)?.groupValues?.drop(1)?.map { it.toLong() }
    ?: error("Android version must be a stable major.minor.patch version")
require(versionParts[0] <= 2099 && versionParts[1] < 1000 && versionParts[2] < 1000)
val appVersionCode = versionParts[0] * 1_000_000 + versionParts[1] * 1000 + versionParts[2]
require(appVersionCode in 1..2_100_000_000)

val signingEnvNames = listOf(
    "HOLON_ANDROID_KEYSTORE_PATH", "HOLON_ANDROID_STORE_PASSWORD",
    "HOLON_ANDROID_KEY_ALIAS", "HOLON_ANDROID_KEY_PASSWORD",
)
val signingValues = signingEnvNames.map { providers.environmentVariable(it).orNull }
require(signingValues.all { it.isNullOrBlank() } || signingValues.all { !it.isNullOrBlank() }) {
    "Android release signing requires all four HOLON_ANDROID signing variables"
}
val hasReleaseSigning = signingValues.all { !it.isNullOrBlank() }

android {
    namespace = "run.holon.android.app"
    compileSdk = 36

    defaultConfig {
        applicationId = "run.holon.android"
        minSdk = 26
        targetSdk = 36
        versionCode = appVersionCode.toInt()
        versionName = appVersion
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    signingConfigs {
        if (hasReleaseSigning) {
            create("officialRelease") {
                storeFile = file(signingValues[0]!!)
                storeType = "JKS"
                storePassword = signingValues[1]
                keyAlias = signingValues[2]
                keyPassword = signingValues[3]
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (hasReleaseSigning) signingConfig = signingConfigs.getByName("officialRelease")
        }
    }

    buildFeatures {
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }

    kotlinOptions {
        jvmTarget = "21"
    }
}

dependencies {
    implementation(project(":sdk"))
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("com.google.android.gms:play-services-code-scanner:16.1.0")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.runtime:runtime")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")
    implementation("androidx.datastore:datastore-preferences:1.2.1")
    implementation("androidx.room:room-runtime:2.8.5")
    implementation("androidx.room:room-ktx:2.8.5")
    kapt("androidx.room:room-compiler:2.8.5")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation("app.nekogram.prism4j:prism4j:2.1.0")
    testImplementation(kotlin("test-junit"))
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.10.2")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test:core:1.6.1")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("androidx.test.uiautomator:uiautomator:2.3.0")
    androidTestImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
    debugImplementation("androidx.compose.ui:ui-tooling")

    val composeBom = platform("androidx.compose:compose-bom:2025.02.00")
    implementation(composeBom)
    androidTestImplementation(composeBom)
}

kapt {
    correctErrorTypes = true
}
