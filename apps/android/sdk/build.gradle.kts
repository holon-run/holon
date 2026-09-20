import java.io.File

plugins {
    `java-library`
    kotlin("jvm")
    kotlin("plugin.serialization")
}

group = "run.holon.android"
version = "0.1.0"

dependencies {
    api("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    testImplementation(kotlin("test-junit"))
    testImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
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

val integrationTest =
    sourceSets.create("integrationTest") {
        compileClasspath += sourceSets.main.get().output
        runtimeClasspath += output + compileClasspath
    }

configurations[integrationTest.implementationConfigurationName].extendsFrom(
    configurations.testImplementation.get(),
)
configurations[integrationTest.runtimeOnlyConfigurationName].extendsFrom(
    configurations.testRuntimeOnly.get(),
)

tasks.register<Test>("integrationTest") {
    description = "Runs the Android SDK against a real Holon daemon"
    group = LifecycleBasePlugin.VERIFICATION_GROUP
    testClassesDirs = integrationTest.output.classesDirs
    classpath = integrationTest.runtimeClasspath
    shouldRunAfter(tasks.test)
    useJUnit()

    val binaryPath =
        providers.gradleProperty("holonTestBinary").orNull
            ?: throw GradleException(
                "Pass -PholonTestBinary=/absolute/path/to/holon",
            )
    val binary = File(binaryPath).absoluteFile
    require(binary.isFile) {
        "Holon test binary does not exist: $binary"
    }
    inputs.file(binary)
    systemProperty("holon.test.binary", binary.absolutePath)
}
