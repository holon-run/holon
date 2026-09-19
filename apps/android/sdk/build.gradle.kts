plugins {
    `java-library`
    kotlin("jvm")
    kotlin("plugin.serialization")
}

group = "run.holon.android"
version = "0.1.0"

dependencies {
    api("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
    testImplementation(kotlin("test-junit"))
}

kotlin {
    jvmToolchain(21)
    sourceSets.main {
        kotlin.srcDir(
            rootProject.file("../../packages/client-wire-kotlin/src/main/kotlin"),
        )
    }
}

sourceSets {
    test {
        resources.srcDir(rootProject.file("../../tests/fixtures/client-wire"))
    }
}
