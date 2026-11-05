package dev.imbo.android.data.remote

import android.content.Context
import android.net.Uri
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import javax.inject.Inject
import javax.inject.Singleton

/** 一张待上传的图片：服务端只收 JPEG / PNG 的原始字节。 */
data class UploadPicture(val bytes: ByteArray, val mime: String)

/**
 * 把相册返回的本地 URI 读成上传用的字节。
 * 抽成接口是为了让 RemoteImboApi 的单测不依赖 Android Context。
 */
fun interface PictureSource {
    suspend fun read(localUri: String): UploadPicture
}

/** 真机实现：Photo Picker 给的是 content://，必须经 ContentResolver 读取。 */
@Singleton
class AndroidPictureSource @Inject constructor(
    @ApplicationContext private val context: Context,
) : PictureSource {

    override suspend fun read(localUri: String): UploadPicture = withContext(Dispatchers.IO) {
        val uri = Uri.parse(localUri)
        val bytes = context.contentResolver.openInputStream(uri)?.use { it.readBytes() }
            ?: throw ImboApiException(400, "image_invalid", "Couldn't read that picture.")
        if (bytes.size > MAX_BYTES) {
            throw ImboApiException(413, "file_too_large", "The picture can be at most 5 MB.")
        }
        val mime = mimeOf(bytes)
            ?: throw ImboApiException(415, "image_invalid", "Only JPEG and PNG pictures can be avatars.")
        UploadPicture(bytes, mime)
    }

    /** 按文件头判断格式，比 content resolver 报的 MIME 更可靠。 */
    private fun mimeOf(bytes: ByteArray): String? = when {
        bytes.size >= 3 && bytes[0] == 0xFF.toByte() && bytes[1] == 0xD8.toByte() &&
            bytes[2] == 0xFF.toByte() -> "image/jpeg"
        bytes.size >= 4 && bytes[0] == 0x89.toByte() && bytes[1] == 0x50.toByte() &&
            bytes[2] == 0x4E.toByte() && bytes[3] == 0x47.toByte() -> "image/png"
        else -> null
    }

    private companion object {
        /** 与服务端 avatar::MAX_BYTES 一致。 */
        const val MAX_BYTES = 5 * 1024 * 1024
    }
}
