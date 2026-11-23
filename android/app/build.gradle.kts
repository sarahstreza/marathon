// :app — the Android client: libmpv playback, the Compose UI (phone and
// TV), input mapping, and lifecycle. Logic lives in :core; see
// docs/ANDROID.md.
plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

/**
 * The key builds are signed with (docs/ANDROID.md "Signing"), from the
 * Gradle property `marathon.signing.<name>` (e.g. in
 * ~/.gradle/gradle.properties) or the environment variable
 * `MARATHON_SIGNING_<NAME>` (CI). Unset or empty: null.
 */
fun signing(name: String, env: String): String? =
    providers.gradleProperty("marathon.signing.$name")
        .orElse(providers.environmentVariable("MARATHON_SIGNING_$env"))
        .orNull
        ?.takeIf { it.isNotBlank() }

val signingStoreFile = signing("storeFile", "STORE_FILE")
val signingPassword = signing("storePassword", "STORE_PASSWORD")
if ((signingStoreFile == null) != (signingPassword == null)) {
    throw GradleException(
        "Signing is half set up: give both marathon.signing.storeFile and marathon.signing.storePassword " +
            "(or MARATHON_SIGNING_STORE_FILE and MARATHON_SIGNING_STORE_PASSWORD), or neither",
    )
}

android {
    namespace = "marathon.android"
    compileSdk = 37

    defaultConfig {
        applicationId = "tv.marathon.android"
        minSdk = 26
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
    }

    // With a Marathon key configured, debug builds are signed with it, so
    // every build (this machine's, CI's) installs over the last one.
    // Without one, they get the machine's own debug key, as usual.
    if (signingStoreFile != null && signingPassword != null) {
        val marathon = signingConfigs.create("marathon") {
            storeFile = file(signingStoreFile)
            storePassword = signingPassword
            keyAlias = signing("keyAlias", "KEY_ALIAS") ?: "marathon"
            // A PKCS12 keystore has one password for the store and its key.
            keyPassword = signing("keyPassword", "KEY_PASSWORD") ?: signingPassword
        }
        buildTypes.getByName("debug").signingConfig = marathon
    }

    buildFeatures {
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    testOptions {
        // Unit tests run on the JVM against android.jar's stubs.
        unitTests.isReturnDefaultValues = true
    }

    lint {
        warningsAsErrors = false
        abortOnError = true
    }

    packaging {
        // The libmpv AAR ships libc++_shared.so; keep one copy.
        jniLibs.pickFirsts.add("**/libc++_shared.so")
    }
}

dependencies {
    implementation(project(":core"))
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.foundation)
    implementation(libs.compose.material3)
    implementation(libs.activity.compose)
    implementation(libs.core.ktx)
    implementation(libs.core.remoteviews)
    implementation(libs.lifecycle.runtime.compose)
    implementation(libs.libmpv)

    testImplementation(platform(libs.junit.bom))
    testImplementation(libs.junit.jupiter)
    testRuntimeOnly(libs.junit.platform.launcher)
}

tasks.withType<Test>().configureEach { useJUnitPlatform() }
