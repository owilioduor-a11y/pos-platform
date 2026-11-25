# Keep UniFFI Kotlin bindings
-keep class uniffi.pos_core.** { *; }

# Keep POS app classes
-keep class com.posplatform.app.** { *; }

# JNI
-keepclasseswithmembernames class * {
    native <methods>;
}
