package dev.imbo.android.push

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import dev.imbo.android.MainActivity
import dev.imbo.android.R

/**
 * 通知栏：一条消息一条通知，点开直达对应会话。
 *
 * 服务端既发 notification 也发 data；App 在后台时系统会用 notification 直接展示
 * （点开是启动 Intent，data 里的 conversation_id 会被带进 extras），
 * 前台时走这里自己构造，保证两种情况的点击行为一致。
 */
object Notifications {
    const val CHANNEL_ID = "imbo"
    const val EXTRA_CONVERSATION = "conversation_id"

    fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        if (manager.getNotificationChannel(CHANNEL_ID) != null) return
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                context.getString(R.string.app_name),
                NotificationManager.IMPORTANCE_HIGH,
            ).apply {
                description = context.getString(R.string.app_name)
                enableVibration(true)
            },
        )
    }

    /** 没有通知权限时什么都不做：请求权限由对话列表负责。 */
    fun canPost(context: Context): Boolean =
        Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED

    fun show(context: Context, title: String?, body: String?, conversationId: Long?) {
        if (!canPost(context)) return
        ensureChannel(context)
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            if (conversationId != null) putExtra(EXTRA_CONVERSATION, conversationId)
        }
        val requestCode = conversationId?.toInt() ?: 0
        val pending = PendingIntent.getActivity(
            context,
            requestCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher)
            .setContentTitle(title ?: context.getString(R.string.app_name))
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setAutoCancel(true)
            .setContentIntent(pending)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .build()
        // 同一个会话的通知互相覆盖，不在通知栏堆一长串
        NotificationManagerCompat.from(context).notify(requestCode, notification)
    }
}
