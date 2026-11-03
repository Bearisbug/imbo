package dev.imbo.android.ui.components

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Typeface
import android.net.Uri
import android.text.Layout
import android.text.StaticLayout
import android.text.TextPaint
import androidx.core.content.FileProvider
import dev.imbo.android.data.Message
import java.io.File
import java.io.FileOutputStream
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * 把选中的几条消息画成一张图，交给系统分享（微信、相册、AirDrop 之类）。
 *
 * 不用 Compose 截图：离屏 Canvas 更容易控制尺寸和排版，也不依赖当前滚动位置。
 * 真机验证只能靠人眼看图，所以这里只做「能出图、能分享」两件事。
 */
object ShareImage {
    private const val WIDTH = 1080
    private const val MARGIN = 56
    private const val GAP = 36
    private const val BG = 0xFF0F0F0E.toInt()
    private const val ON_BG = 0xFFEDEAE2.toInt()
    private const val TERTIARY = 0xFFA19D93.toInt()

    /** 画好写到 cache/share 下，返回可分享的 Intent；失败返回 null。 */
    fun shareIntent(context: Context, title: String, messages: List<Message>): Intent? {
        val bitmap = runCatching { render(title, messages) }.getOrNull() ?: return null
        val dir = File(context.cacheDir, "share").apply { mkdirs() }
        val file = File(dir, "imbo-${System.currentTimeMillis()}.png")
        runCatching {
            FileOutputStream(file).use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
        }.getOrElse { return null }
        bitmap.recycle()
        val uri: Uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
        return Intent(Intent.ACTION_SEND).apply {
            type = "image/png"
            putExtra(Intent.EXTRA_STREAM, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
    }

    private fun render(title: String, messages: List<Message>): Bitmap {
        val bodyPaint = TextPaint().apply {
            color = ON_BG
            textSize = 34f
            typeface = Typeface.MONOSPACE
            isAntiAlias = true
        }
        val metaPaint = TextPaint().apply {
            color = TERTIARY
            textSize = 26f
            typeface = Typeface.MONOSPACE
            isAntiAlias = true
        }
        val titlePaint = TextPaint().apply {
            color = ON_BG
            textSize = 44f
            typeface = Typeface.MONOSPACE
            isAntiAlias = true
        }
        val plainPaint = Paint().apply {
            color = TERTIARY
            textSize = 26f
            typeface = Typeface.MONOSPACE
            isAntiAlias = true
        }

        val textWidth = WIDTH - MARGIN * 2
        val clock = SimpleDateFormat("HH:mm", Locale.getDefault())
        val day = SimpleDateFormat("yyyy.MM.dd", Locale.getDefault()).format(Date())

        // 先量一遍高度，再一次性建 bitmap：StaticLayout 的 height 要等换行算完才知道
        val blocks = messages.map { message ->
            val layout = StaticLayout.Builder
                .obtain(message.body.ifBlank { "（附件）" }, 0, message.body.ifBlank { "（附件）" }.length, bodyPaint, textWidth)
                .setAlignment(Layout.Alignment.ALIGN_NORMAL)
                .build()
            layout
        }
        val headerHeight = 150
        val footerHeight = 80
        var height = headerHeight + footerHeight + MARGIN
        blocks.forEach { height += it.height + GAP }

        val bitmap = Bitmap.createBitmap(WIDTH, height, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        canvas.drawColor(BG)

        var y = MARGIN.toFloat()
        canvas.drawText(title, MARGIN.toFloat(), y + 40, titlePaint)
        y += 70
        canvas.drawText(day, MARGIN.toFloat(), y + 30, plainPaint)
        y += 80

        messages.forEachIndexed { index, message ->
            val who = if (message.mine) "我" else message.senderName
            val meta = "$who · ${clock.format(Date(message.createdAt))}"
            canvas.drawText(meta, MARGIN.toFloat(), y + 28, metaPaint)
            y += 44
            canvas.save()
            canvas.translate(MARGIN.toFloat(), y)
            blocks[index].draw(canvas)
            canvas.restore()
            y += blocks[index].height + GAP
        }

        canvas.drawText("imbo.dev", MARGIN.toFloat(), y + 30, plainPaint)
        return bitmap
    }
}
