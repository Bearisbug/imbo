package dev.imbo.android.data.remote

import android.content.Context
import android.net.Uri
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * 读取本机 URI 的内容。选中文件时拿到的是 content:// URI，只能通过 ContentResolver 打开，
 * 所以把它抽成接口：真实实现依赖 Android Context，单测可以注入假的字节来源。
 */
fun interface LocalFileSource {
    suspend fun read(localUri: String): ByteArray
}

/** 生产实现：content:// 与 file:// 都走 ContentResolver，读取放 IO 线程。 */
class ContentResolverFileSource(private val context: Context) : LocalFileSource {
    override suspend fun read(localUri: String): ByteArray = withContext(Dispatchers.IO) {
        val uri = Uri.parse(localUri)
        context.contentResolver.openInputStream(uri)?.use { it.readBytes() }
            ?: throw ImboApiException(400, "file_unreadable", "Can't read the selected file.")
    }
}
