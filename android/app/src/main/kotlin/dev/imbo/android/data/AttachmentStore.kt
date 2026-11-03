package dev.imbo.android.data

import java.io.File

/**
 * 附件在本机的落地文件。上传后把原字节存一份，收到别人的图片时按需下载再存一份，
 * 这样消息从服务端重新拉取后仍然能显示图片（服务端只返回文件 id）。
 *
 * 只依赖一个目录，方便单测用临时目录构造。
 */
class AttachmentStore(private val dir: File) {

    init {
        dir.mkdirs()
    }

    /** 写一份字节，返回 file:// URI，供 Coil 直接加载。 */
    fun save(id: String, bytes: ByteArray, name: String): String {
        val file = File(dir, safeName(id, name))
        file.writeBytes(bytes)
        return "file://" + file.absolutePath
    }

    /** 已经存在就返回路径，没有就返回 null。 */
    fun find(id: String): String? {
        val file = dir.listFiles()?.firstOrNull { it.name.substringBefore('.') == id } ?: return null
        return if (file.exists() && file.length() > 0) "file://" + file.absolutePath else null
    }

    fun has(id: String): Boolean = find(id) != null

    private fun safeName(id: String, name: String): String {
        val ext = name.substringAfterLast('.', "").lowercase().take(5)
        return if (ext.isNotEmpty()) "$id.$ext" else id
    }
}
