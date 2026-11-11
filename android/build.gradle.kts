// Plugins are declared here once, with their versions from the catalog,
// and applied in the modules that use them.
plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.kotlin.jvm) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.openapi.generator) apply false
}
