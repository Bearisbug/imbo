package dev.imbo.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.imbo.android.ui.theme.ImboColor
import dev.imbo.android.ui.theme.ImboDimen
import dev.imbo.android.ui.theme.ImboMono

@Composable
fun ScreenRoot(content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(ImboColor.Background)
            .windowInsetsPadding(WindowInsets.safeDrawing),
        content = content,
    )
}

@Composable
fun InsetField(
    label: String,
    value: String,
    placeholder: String,
    onValueChange: (String) -> Unit,
    keyboardType: KeyboardType = KeyboardType.Text,
    hint: String? = null,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxWidth()) {
        Text(
            text = label,
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
            fontWeight = FontWeight.Medium,
        )
        Spacer(Modifier.height(6.dp))
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(ImboDimen.Field)
                .clip(RoundedCornerShape(6.dp))
                .background(ImboColor.TertiaryContainer)
                .border(1.dp, ImboColor.Outline, RoundedCornerShape(6.dp))
                .padding(horizontal = 12.dp),
            contentAlignment = Alignment.CenterStart,
        ) {
            BasicTextField(
                value = value,
                onValueChange = onValueChange,
                singleLine = true,
                textStyle = TextStyle(color = ImboColor.OnBackground, fontFamily = ImboMono, fontSize = 14.sp),
                cursorBrush = SolidColor(ImboColor.Primary),
                keyboardOptions = KeyboardOptions(keyboardType = keyboardType),
                modifier = Modifier.fillMaxWidth(),
                decorationBox = { inner ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (value.isEmpty()) {
                            Text(placeholder, color = ImboColor.Tertiary, fontFamily = ImboMono, fontSize = 14.sp)
                        }
                        inner()
                    }
                },
            )
        }
        if (hint != null) {
            Spacer(Modifier.height(6.dp))
            Text(hint, color = ImboColor.Tertiary, fontFamily = ImboMono, fontSize = 12.sp)
        }
    }
}

@Composable
fun HSpacer(width: Int) = Spacer(Modifier.width(width.dp))
