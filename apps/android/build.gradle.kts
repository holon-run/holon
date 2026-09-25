plugins {
    // alpha02 targets the Gradle 8 / AGP 8 line used by this project. Newer
    // Paparazzi alphas resolve AGP 8.13.2 tooling that is not published for
    // this build combination.
    id("app.cash.paparazzi") version "2.0.0-alpha02" apply false
    id("com.android.application") version "8.13.0" apply false
    id("org.jetbrains.kotlin.android") version "2.2.20" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.20" apply false
    id("org.jetbrains.kotlin.kapt") version "2.2.20" apply false
    kotlin("jvm") version "2.2.20" apply false
    kotlin("plugin.serialization") version "2.2.20" apply false
}
