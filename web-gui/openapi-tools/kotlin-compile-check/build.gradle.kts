plugins {
    kotlin("jvm") version "2.2.20"
    kotlin("plugin.serialization") version "2.2.20"
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
}

kotlin {
    jvmToolchain(21)
    sourceSets.main {
        kotlin.srcDir(
            rootDir.resolve("../../../packages/client-wire-kotlin/src/main/kotlin"),
        )
    }
}
