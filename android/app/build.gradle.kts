import java.util.UUID
import org.gradle.api.GradleException

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

/** Environment variables are preferred; -P properties are the local fallback. */
fun posConfig(propertyName: String, envVar: String): String? =
    listOf(System.getenv(envVar), project.findProperty(propertyName)?.toString())
        .firstNotNullOfOrNull { raw -> raw?.trim()?.takeIf { it.isNotEmpty() } }

/** Escape a value so it can be emitted as a Java/Kotlin string literal in BuildConfig. */
fun buildConfigString(value: String): String = buildString {
    append('"')
    value.forEach { ch ->
        when (ch) {
            '\\' -> append("\\\\")
            '"' -> append("\\\"")
            '\n' -> append("\\n")
            '\r' -> append("\\r")
            else -> append(ch)
        }
    }
    append('"')
}

fun requireReleaseConfig(serverUrl: String?, apiToken: String?, merchantId: String?) {
    val missing = buildList {
        if (serverUrl.isNullOrEmpty()) add("POS_SERVER_URL (-PposServerUrl)")
        if (apiToken.isNullOrEmpty()) add("POS_API_TOKEN (-PposApiToken)")
        if (merchantId.isNullOrEmpty()) add("POS_MERCHANT_ID (-PposMerchantId)")
    }
    if (missing.isNotEmpty()) {
        throw GradleException("Release builds require: ${missing.joinToString(", ")}")
    }
    val url = requireNotNull(serverUrl)
    val token = requireNotNull(apiToken)
    val merchant = requireNotNull(merchantId)
    if (!url.startsWith("https://", ignoreCase = true)) {
        throw GradleException("Release POS_SERVER_URL must use https://")
    }
    if (token == "dev-token" || token.length < 32) {
        throw GradleException("Release POS_API_TOKEN must be a real token (at least 32 characters)")
    }
    runCatching { UUID.fromString(merchant) }.onFailure {
        throw GradleException("Release POS_MERCHANT_ID must be a valid UUID", it)
    }
}

val posServerUrl = posConfig("posServerUrl", "POS_SERVER_URL")
val posApiToken = posConfig("posApiToken", "POS_API_TOKEN")
val posMerchantId = posConfig("posMerchantId", "POS_MERCHANT_ID")

// Debug defaults stay on the emulator host; release values stay empty unless injected.
val debugServerUrl = posServerUrl ?: "http://10.0.2.2:8080"
val debugApiToken = posApiToken ?: "dev-token"
val debugMerchantId = posMerchantId ?: "00000000-0000-7000-8000-000000000001"

val validateReleaseConfig by tasks.registering {
    description = "Fails a release build when production POS config is missing or unsafe."
    group = "verification"
    doLast {
        requireReleaseConfig(posServerUrl, posApiToken, posMerchantId)
    }
}

// Only release variants need production configuration; assembleDebug stays self-contained.
tasks.configureEach {
    if (name in setOf("processReleaseResources", "assembleRelease", "bundleRelease")) {
        dependsOn(validateReleaseConfig)
    }
}

android {
    namespace = "com.posplatform.app"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.posplatform.app"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        buildConfigField("String", "SERVER_URL", buildConfigString(debugServerUrl))
        buildConfigField("String", "API_TOKEN", buildConfigString(debugApiToken))
        buildConfigField("String", "MERCHANT_ID", buildConfigString(debugMerchantId))
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            // Empty unless injected at build time; validateReleaseConfig blocks such builds.
            buildConfigField("String", "SERVER_URL", buildConfigString(posServerUrl.orEmpty()))
            buildConfigField("String", "API_TOKEN", buildConfigString(posApiToken.orEmpty()))
            buildConfigField("String", "MERCHANT_ID", buildConfigString(posMerchantId.orEmpty()))
        }
        debug {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.09.03")
    implementation(composeBom)
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.activity:activity-compose:1.9.2")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.6")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.6")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    debugImplementation("androidx.compose.ui:ui-tooling")

    // Barcode scanning
    implementation("com.google.mlkit:barcode-scanning:17.2.0")
    implementation("androidx.camera:camera-camera2:1.3.4")
    implementation("androidx.camera:camera-lifecycle:1.3.4")
    implementation("androidx.camera:camera-view:1.3.4")

    // Coroutines
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")

    // UniFFI-generated Kotlin bindings (JNI bridge to pos-core)
    implementation("net.java.dev.jna:jna:5.15.0@aar")
}
