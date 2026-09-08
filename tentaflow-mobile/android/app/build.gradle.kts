plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val nativeAbis = listOf("arm64-v8a", "armeabi-v7a", "x86_64").filter { abi ->
    file("src/main/jniLibs/$abi/libtentaflow_mobile.so").isFile
}

android {
    namespace = "ai.tentaflow.mobile"
    compileSdk = 34

    defaultConfig {
        applicationId = "ai.tentaflow.mobile"
        minSdk = 26
        targetSdk = 34
        versionCode = 2
        versionName = "0.2.0-beta"

        ndk {
            abiFilters += nativeAbis
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    packaging {
        jniLibs.excludes += setOf("**/libiroh-*.so", "**/libiroh_relay-*.so")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }
}

tasks.configureEach {
    if (name.startsWith("merge") && name.endsWith("NativeLibs")) {
        doFirst {
            check(nativeAbis.isNotEmpty()) {
                "Brak JNI TentaFlow. Uruchom scripts/build-rust.sh przed pakowaniem APK."
            }
        }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.12.0")
    implementation("androidx.appcompat:appcompat:1.6.1")
    implementation("com.google.android.material:material:1.11.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.7.0")
    // Positioning sensors: fused GPS + ARCore depth (phone as a sensor-robot).
    implementation("com.google.android.gms:play-services-location:21.3.0")
    implementation("com.google.ar:core:1.45.0")
}
