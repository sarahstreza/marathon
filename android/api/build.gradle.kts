// :api — the Kotlin client for Marathon's HTTP API, generated at build
// time from openapi.json (a committed snapshot of `marathond
// --print-openapi`). Generated code lives under build/ and is never
// committed. Refresh the snapshot with scripts/gen-android-api.sh.
//
// Action and Event (internally tagged enums, `oneOf` + discriminator in
// the spec) are written by hand in :core instead, and checked against the
// Rust JSON fixtures there; see docs/ANDROID.md.
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.openapi.generator)
}

val generatedDir = layout.buildDirectory.dir("generated/openapi")

openApiGenerate {
    generatorName.set("kotlin")
    inputSpec.set(layout.projectDirectory.file("openapi.json").asFile.path)
    outputDir.set(generatedDir.get().asFile.path)
    packageName.set("marathon.api")
    library.set("jvm-okhttp4")
    generateApiTests.set(false)
    generateModelTests.set(false)
    generateApiDocumentation.set(false)
    generateModelDocumentation.set(false)
    ignoreFileOverride.set(layout.projectDirectory.file(".openapi-generator-ignore").asFile.path)
    // Start clean so files dropped from the spec (or newly ignored) go away.
    cleanupOutput.set(true)
    configOptions.set(
        mapOf(
            "serializationLibrary" to "kotlinx_serialization",
            "sourceFolder" to "src/main/kotlin",
            "omitGradleWrapper" to "true",
            "omitGradlePluginVersions" to "true",
            "enumPropertyNaming" to "UPPERCASE",
        ),
    )
}

sourceSets.main {
    kotlin.srcDir(generatedDir.map { it.dir("src/main/kotlin") })
}

tasks.named("compileKotlin") { dependsOn(tasks.named("openApiGenerate")) }

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        // Generated code is not ours to lint.
        suppressWarnings.set(true)
    }
}

dependencies {
    api(libs.kotlinx.serialization.json)
    api(libs.okhttp)
}
