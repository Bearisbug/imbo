# 发布包（R8）保留规则。默认的 proguard-android-optimize.txt 已包含 Android 基础规则，
# 这里只补「反射/序列化/注解」会被 R8 删掉的部分。

# --- kotlinx.serialization ---
# @Serializable 生成的 serializer 是按类名反射找的，混淆后名字对不上就崩
-keepattributes *Annotation*, InnerClasses, Signature, RuntimeVisibleAnnotations, RuntimeVisibleParameterAnnotations, AnnotationDefault
-keepclassmembers class kotlinx.serialization.json.** {
    *** Companion;
}
-keepclasseswithmembers class kotlinx.serialization.json.** {
    kotlinx.serialization.KSerializer serializer(...);
}
-keep,includedescriptorclasses class dev.imbo.android.**$$serializer { *; }
-keepclassmembers class dev.imbo.android.** {
    *** Companion;
}
-keepclasseswithmembers class dev.imbo.android.** {
    kotlinx.serialization.KSerializer serializer(...);
}
# 枚举序列化（Delivery / Access 等）依赖 valueOf
-keepclassmembers enum dev.imbo.android.** {
    public static **[] values();
    public static ** valueOf(java.lang.String);
}

# --- OkHttp / Okio ---
-dontwarn okhttp3.**
-dontwarn okio.**
-dontwarn org.conscrypt.**
-dontwarn org.bouncycastle.**
-dontwarn org.openjsse.**
-keepclassmembers class okhttp3.internal.** { *; }

# --- Coil ---
-dontwarn coil.**
-keep class coil.** { *; }

# --- Hilt / Dagger ---
# Hilt 生成的组件由注解处理器生成，R8 会保留注解标注的类型；这里只兜底
-keep,allowobfuscation @interface dagger.hilt.**
-keep class dagger.hilt.** { *; }
-keep class javax.inject.** { *; }

# --- Firebase / FCM ---
# 推送服务由系统按类名启动，混淆后名字变了就起不来
-keep class dev.imbo.android.push.ImboMessagingService { *; }
-keep class com.google.firebase.** { *; }
-dontwarn com.google.firebase.**
-keep class com.google.android.gms.** { *; }
-dontwarn com.google.android.gms.**

# --- 其它 ---
# 保留行号，线上崩溃栈还能看
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile
