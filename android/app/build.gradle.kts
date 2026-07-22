plugins {
    id("com.android.application")
}

android {
    namespace = "dev.palmtop.spike"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.palmtop.spike"
        minSdk = 30
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    signingConfigs {
        // Reuses the same debug keystore the old manual build.sh generated,
        // so `adb install -r` over an existing install doesn't hit a
        // signature mismatch. Generated on demand if missing.
        create("palmtopDebug") {
            storeFile = file("../../android-spike/debug.keystore")
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }
    }

    buildTypes {
        debug {
            signingConfig = signingConfigs.getByName("palmtopDebug")
        }
    }
}

dependencies {
    // LatencyTracker is deliberately free of Android imports so its
    // clock-offset math runs under a plain JVM test. That math is standard NTP
    // and exactly the kind of arithmetic that can be wrong by a sign or a
    // factor of two while still producing believable-looking numbers on a
    // running device -- which is worse than producing none, because a
    // plausible number gets quoted.
    testImplementation("junit:junit:4.13.2")

    // Signal's fork of the reference Noise Protocol Java implementation --
    // used for the transport encryption layer (see NoiseTransport.java).
    // Chosen over vendoring a raw jar specifically so it resolves normally
    // through Gradle like any other dependency, which is the whole reason
    // this project moved off the manual aapt2/d8 build.
    implementation("org.signal.forks:noise-java:0.1.1")

    // In-app QR scanning for pairing (host:port:token:pubkey) -- the other
    // reason this project needed real dependency resolution instead of
    // vendored jars. See QrScanActivity.java.
    //
    // androidx.activity:activity (not just camera-lifecycle) is needed
    // because CameraX's bindToLifecycle() requires a LifecycleOwner, which
    // the app's existing plain android.app.Activity (MainActivity) doesn't
    // implement -- ComponentActivity does. QrScanActivity extends it
    // specifically for this; MainActivity is left as a plain Activity.
    implementation("androidx.activity:activity:1.13.0")
    val cameraxVersion = "1.6.1"
    implementation("androidx.camera:camera-core:$cameraxVersion")
    implementation("androidx.camera:camera-camera2:$cameraxVersion")
    implementation("androidx.camera:camera-lifecycle:$cameraxVersion")
    implementation("androidx.camera:camera-view:$cameraxVersion")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")
}
