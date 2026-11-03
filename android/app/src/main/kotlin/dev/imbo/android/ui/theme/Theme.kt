package dev.imbo.android.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.imbo.android.R

/** 全部取自官方线上站点，禁止在此之外出现原始色值。 */
object ImboColor {
    val Background = Color(0xFF0F0F0E)
    val OnBackground = Color(0xFFEDEAE2)
    val Primary = Color(0xFFEDEAE2)
    val OnPrimary = Color(0xFF0F0F0E)
    val SecondaryContainer = Color(0xFF312F2B)
    val OnSecondaryContainer = Color(0xFFEDEAE2)
    val Tertiary = Color(0xFFA19D93)
    val TertiaryContainer = Color(0xFF080707)
    val Outline = Color(0xFF838077)
    val OutlineVariant = Color(0xFF312F2B)
    val Error = Color(0xFFD91E36)
    val OnError = Color(0xFFEDEAE2)
    val Success = Color(0xFF5BCC80)
}

object ImboDimen {
    val Gutter = 16.dp
    val BrandBar = 48.dp
    val TabRow = 40.dp
    val ListRow = 58.dp
    val Avatar = 32.dp
    val AvatarLarge = 64.dp
    val BubbleRadius = 8.dp
    val Composer = 44.dp
    val Field = 40.dp
    val Button = 40.dp
    val Chip = 24.dp
    val TouchTarget = 48.dp
    val Hairline = 1.dp
}

/** 官方正文字体就是 JetBrains Mono，中文按字形回落系统（与官方 Web 一致）。 */
val ImboMono: FontFamily = FontFamily(
    Font(R.font.jetbrains_mono_regular, FontWeight.Normal),
    Font(R.font.jetbrains_mono_medium, FontWeight.Medium),
    Font(R.font.jetbrains_mono_semibold, FontWeight.SemiBold),
)

/** 官方正文 14sp / 行高 25.2sp，说明 12sp，分区标题 13sp，页面大标题 22sp。 */
val ImboType = Typography(
    headlineMedium = TextStyle(fontFamily = ImboMono, fontSize = 22.sp, lineHeight = 28.sp, fontWeight = FontWeight.SemiBold),
    titleSmall = TextStyle(fontFamily = ImboMono, fontSize = 14.sp, lineHeight = 21.sp, fontWeight = FontWeight.SemiBold),
    bodyMedium = TextStyle(fontFamily = ImboMono, fontSize = 14.sp, lineHeight = 25.2.sp, fontWeight = FontWeight.Normal),
    bodySmall = TextStyle(fontFamily = ImboMono, fontSize = 12.sp, lineHeight = 17.sp, fontWeight = FontWeight.Normal),
    labelLarge = TextStyle(fontFamily = ImboMono, fontSize = 14.sp, lineHeight = 20.sp, fontWeight = FontWeight.SemiBold),
    labelMedium = TextStyle(fontFamily = ImboMono, fontSize = 13.sp, lineHeight = 18.sp, fontWeight = FontWeight.Medium),
    labelSmall = TextStyle(fontFamily = ImboMono, fontSize = 12.sp, lineHeight = 16.sp, fontWeight = FontWeight.Normal),
)

private val ImboScheme = darkColorScheme(
    primary = ImboColor.Primary,
    onPrimary = ImboColor.OnPrimary,
    background = ImboColor.Background,
    onBackground = ImboColor.OnBackground,
    surface = ImboColor.Background,
    onSurface = ImboColor.OnBackground,
    surfaceVariant = ImboColor.SecondaryContainer,
    onSurfaceVariant = ImboColor.Tertiary,
    secondaryContainer = ImboColor.SecondaryContainer,
    onSecondaryContainer = ImboColor.OnSecondaryContainer,
    tertiary = ImboColor.Tertiary,
    tertiaryContainer = ImboColor.TertiaryContainer,
    outline = ImboColor.Outline,
    outlineVariant = ImboColor.OutlineVariant,
    error = ImboColor.Error,
    onError = ImboColor.OnError,
)

@Composable
fun ImboTheme(content: @Composable () -> Unit) {
    // 产品只有暗色主题，与官方一致；不提供亮色分支。
    @Suppress("UNUSED_EXPRESSION")
    isSystemInDarkTheme()
    MaterialTheme(
        colorScheme = ImboScheme,
        typography = ImboType,
        content = content,
    )
}
