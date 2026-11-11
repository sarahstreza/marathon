// :core — the Android client's port of crates/marathon-client: the Tuner
// state machine, Player/Api seams, navigation, digit entry, overlay cues,
// the action bus, and the remote control channel. Pure Kotlin/JVM, with no
// Android dependency, so its tests run on a plain JVM (docs/ANDROID.md).
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        allWarningsAsErrors.set(true)
    }
}

dependencies {
    api(project(":api"))
    api(libs.kotlinx.serialization.json)
    api(libs.okhttp)

    testImplementation(platform(libs.junit.bom))
    testImplementation(libs.junit.jupiter)
    testImplementation(libs.okhttp.mockwebserver)
    testRuntimeOnly(libs.junit.platform.launcher)
}

tasks.test {
    useJUnitPlatform()
    // The Action/Event JSON fixtures written by the Rust side
    // (crates/marathon-api/tests/control_fixtures.rs).
    systemProperty(
        "marathon.controlFixture",
        rootProject.layout.projectDirectory
            .file("../crates/marathon-api/tests/fixtures/control.json").asFile.path,
    )
    inputs.file("../../crates/marathon-api/tests/fixtures/control.json")
}
