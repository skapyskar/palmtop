plugins {
    id("com.android.application")
}

android {
    namespace = "dev.palmtop.client"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.palmtop.client"
        minSdk = 30
        targetSdk = 36
        // Taken from the release tag when CI builds one, so an installed APK
        // can be identified. Every release so far reported "0.1.0"/1 no matter
        // which release it came from, which made "is this phone running an old
        // build?" -- the first question worth asking when one device
        // misbehaves and another does not -- unanswerable from the phone.
        val palmtopTag = (System.getenv("PALMTOP_VERSION") ?: "").removePrefix("v")
        versionName = palmtopTag.ifEmpty { "0.0.0-dev" }
        // Monotonic and derived from the tag: 0.2.2 -> 2002. Android refuses to
        // install an APK whose versionCode is below the installed one, so this
        // must never go backwards between releases.
        versionCode = Regex("^(\\d+)\\.(\\d+)\\.(\\d+)").find(palmtopTag)?.destructured
            ?.let { (major, minor, patch) ->
                major.toInt() * 1_000_000 + minor.toInt() * 1_000 + patch.toInt()
            } ?: 1
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    signingConfigs {
        // Reuses the debug keystore the old manual build.sh generated, so
        // `adb install -r` over an existing install on a development phone
        // doesn't hit a signature mismatch.
        //
        // Conditional because that keystore is deliberately not committed:
        // on a fresh clone or in CI it simply isn't there, and an
        // unconditional reference would fail the build at configuration time
        // for someone who only wanted to compile. Absent it, AGP falls back
        // to its own auto-generated debug key, which is fine -- the only
        // cost is having to uninstall before reinstalling on a phone that
        // already has a differently-signed debug build.
        val debugKeystore = file("../../android-spike/debug.keystore")
        if (debugKeystore.exists()) {
            create("palmtopDebug") {
                storeFile = debugKeystore
                storePassword = "android"
                keyAlias = "androiddebugkey"
                keyPassword = "android"
            }
        }

        // Release signing, configured entirely from the environment.
        //
        // No key material is committed, and none can be: the keystore is
        // decoded from a secret at build time in CI and the file itself is
        // gitignored locally. That matters more than usual for Android --
        // the signing key *is* the app's identity to every device that has
        // ever installed it, so a leaked key lets anyone ship a malicious
        // "update", and a lost key means never being able to update the app
        // again for existing users. It cannot be rotated.
        //
        // Falls through to null when the env vars are absent, so an ordinary
        // `assembleDebug` on a fresh clone still works with no setup.
        val keystorePath = System.getenv("PALMTOP_KEYSTORE")
        if (keystorePath != null && file(keystorePath).exists()) {
            create("palmtopRelease") {
                storeFile = file(keystorePath)
                storePassword = System.getenv("PALMTOP_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("PALMTOP_KEY_ALIAS")
                keyPassword = System.getenv("PALMTOP_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        debug {
            // findByName, not getByName: the shared debug keystore is
            // optional (see above), and null here means AGP's own default.
            signingConfigs.findByName("palmtopDebug")?.let { signingConfig = it }
        }
        release {
            // Unsigned when no keystore is configured, which is a deliberate,
            // visible failure: an APK accidentally signed with the *debug*
            // key would install and run perfectly well while being
            // permanently un-updatable by a properly signed release, and
            // nothing about it would look wrong until far too late.
            signingConfig = signingConfigs.findByName("palmtopRelease")

            // Left off on purpose. R8 would strip the reflective entry points
            // that Noise, CameraX and ML Kit rely on, and this app is not
            // large enough for the size win to be worth debugging obfuscated
            // stack traces from users. Revisit only with a tested keep-rules
            // file, never as a default.
            isMinifyEnabled = false
            isShrinkResources = false
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

    // A real org.json for local unit tests. The org.json bundled in Android's
    // framework is *stubbed* in the unit-test android.jar -- every method
    // throws "not mocked" rather than doing anything -- so PairedDevice's
    // serialisation could not otherwise be tested off-device at all. This is
    // the same API, so nothing about the production code changes; it only
    // gives the JVM tests a working implementation to run against.
    testImplementation("org.json:json:20240303")

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
