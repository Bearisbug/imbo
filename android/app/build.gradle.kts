import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
    id("com.google.devtools.ksp")
    id("com.google.dagger.hilt.android")
}


// 发布签名：口令与密钥文件放在 android/keystore.properties（已 gitignore）。
// 没有这个文件时 release 仍然能构建，只是不签名，方便别人 fork 后直接跑。
val keystorePropsFile = rootProject.file("keystore.properties")
val keystoreProps = Properties().apply {
    if (keystorePropsFile.exists()) keystorePropsFile.inputStream().use { load(it) }
}

android {
    namespace = "dev.imbo.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.imbo.android"
        minSdk = 26
        targetSdk = 35
        versionCode = 2
        versionName = "1.0.0"
        resourceConfigurations += listOf("en", "zh")

        // 默认走 mock；-Pimbo.useMock=false -Pimbo.baseUrl=https://… 切真实服务端
        buildConfigField(
            "boolean",
            "USE_MOCK",
            (project.findProperty("imbo.useMock") ?: "true").toString(),
        )
        buildConfigField(
            "String",
            "BASE_URL",
            "\"${project.findProperty("imbo.baseUrl") ?: ""}\"",
        )
        // ntfy 实例地址：账号页据此拼出订阅链接，-Pimbo.ntfyUrl=http://host:8081
        buildConfigField(
            "String",
            "NTFY_URL",
            "\"" + (project.findProperty("imbo.ntfyUrl") ?: "") + "\"",
        )
    }

    signingConfigs {
        if (keystorePropsFile.exists()) {
            create("release") {
                storeFile = file(keystoreProps.getProperty("storeFile"))
                storePassword = keystoreProps.getProperty("storePassword")
                keyAlias = keystoreProps.getProperty("keyAlias")
                keyPassword = keystoreProps.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        debug {
            isMinifyEnabled = true
        }
        release {
            // 发布包开混淆与资源压缩；keep 规则见 proguard-rules.pro
            isMinifyEnabled = true
            isShrinkResources = true
            signingConfig = signingConfigs.findByName("release")
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
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
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }

    testOptions {
        unitTests.all {
            // 工程路径含中文时，测试 JVM 按系统 ANSI 代码页解析 classpath 会失败。
            // 这里尽量对齐编码，但 JDK 17 下仍不足以修复，正式跑测试用 scripts/run-tests.ps1。
            it.jvmArgs("-Dfile.encoding=UTF-8", "-Dsun.jnu.encoding=UTF-8")
            it.systemProperty("file.encoding", "UTF-8")
        }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.10.01")
    implementation(composeBom)
    androidTestImplementation(composeBom)

    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")

    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")

    implementation("androidx.navigation:navigation-compose:2.8.4")

    implementation("com.google.dagger:hilt-android:2.52")
    ksp("com.google.dagger:hilt-android-compiler:2.52")
    implementation("androidx.hilt:hilt-navigation-compose:1.2.0")

    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.7.3")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("io.coil-kt:coil-compose:2.7.0")


    debugImplementation("androidx.compose.ui:ui-tooling")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.8.1")
    testImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
}

