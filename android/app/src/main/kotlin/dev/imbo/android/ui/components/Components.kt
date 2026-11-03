package dev.imbo.android.ui.components

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.runtime.getValue
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.InsertDriveFile
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.outlined.ChevronLeft
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.People
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coil.compose.AsyncImage
import dev.imbo.android.R
import dev.imbo.android.data.Attachment
import dev.imbo.android.data.Conversation
import dev.imbo.android.data.Delivery
import dev.imbo.android.data.Message
import dev.imbo.android.ui.theme.ImboColor
import dev.imbo.android.ui.theme.ImboDimen
import dev.imbo.android.ui.theme.ImboMono

@Composable
fun Wordmark(modifier: Modifier = Modifier) {
    androidx.compose.foundation.Image(
        painter = painterResource(R.drawable.imbo_wordmark),
        contentDescription = stringResource(R.string.app_name),
        modifier = modifier.height(28.dp),
    )
}

@Composable
fun Avatar(letter: String, modifier: Modifier = Modifier, size: Int = 32, group: Boolean = false) {
    Box(
        modifier = modifier
            .size(size.dp)
            .clip(CircleShape)
            .background(ImboColor.SecondaryContainer),
        contentAlignment = Alignment.Center,
    ) {
        if (group) {
            Icon(Icons.Outlined.People, contentDescription = null, tint = ImboColor.OnSecondaryContainer, modifier = Modifier.size(16.dp))
        } else {
            Text(letter, color = ImboColor.OnSecondaryContainer, fontFamily = ImboMono, fontSize = 12.sp, fontWeight = FontWeight.Medium)
        }
    }
}

@Composable
fun AvatarImage(
    letter: String,
    url: String?,
    modifier: Modifier = Modifier,
    size: Int = 32,
    group: Boolean = false,
) {
    val hasPicture = !url.isNullOrBlank()
    Box(
        modifier = modifier
            .size(size.dp)
            .clip(CircleShape)
            .background(ImboColor.SecondaryContainer),
        contentAlignment = Alignment.Center,
    ) {
        if (hasPicture) {
            // 地址失效时 Coil 什么都不画，底下就是同色底，不需要额外兜底
            AsyncImage(
                model = url,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier
                    .size(size.dp)
                    .clip(CircleShape),
            )
        } else if (group) {
            Icon(Icons.Outlined.People, contentDescription = null, tint = ImboColor.OnSecondaryContainer, modifier = Modifier.size(16.dp))
        } else {
            Text(letter, color = ImboColor.OnSecondaryContainer, fontFamily = ImboMono, fontSize = 12.sp, fontWeight = FontWeight.Medium)
        }
    }
}

@Composable
fun UnreadBadge(count: Int, small: Boolean = false) {
    val size = if (small) 16.dp else 20.dp
    val fontSize = if (small) 10.sp else 11.sp
    Box(
        modifier = Modifier
            .height(size)
            .widthIn(min = size)
            .clip(CircleShape)
            .background(ImboColor.Error)
            // 两位数以上自动变胶囊，避免撑爆圆形（背景必须在 padding 之外）
            .then(if (count >= 10) Modifier.padding(horizontal = 4.dp) else Modifier),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = count.toString(),
            color = ImboColor.OnError,
            fontFamily = ImboMono,
            fontSize = fontSize,
            fontWeight = FontWeight.Bold,
            maxLines = 1,
            textAlign = TextAlign.Center,
            style = TextStyle(
                // 默认的字体上下留白会把数字顶偏，去掉后行高等于字形高度，居中才准
                platformStyle = PlatformTextStyle(includeFontPadding = false),
                lineHeightStyle = LineHeightStyle(
                    alignment = LineHeightStyle.Alignment.Center,
                    trim = LineHeightStyle.Trim.Both,
                ),
            ),
        )
    }
}

@Composable
fun Hairline(modifier: Modifier = Modifier, vertical: Boolean = true) {
    Box(
        modifier = modifier
            .then(if (vertical) Modifier.width(1.dp).height(10.dp) else Modifier.height(1.dp).fillMaxWidth())
            .background(ImboColor.OutlineVariant),
    )
}

@Composable
fun BrandBar(
    onSubscribe: () -> Unit,
    onAccount: () -> Unit,
    viewModel: BrandBarViewModel = hiltViewModel(),
) {
    val account by viewModel.account.collectAsStateWithLifecycle()
    // 文案跟着账号真实状态走：试用显示剩余天数，已订阅显示「已订阅」，free / 过期不显示
    val status = when (val access = account?.access) {
        is dev.imbo.android.data.Access.Trial -> stringResource(R.string.trial_days_left, access.daysLeft)
        is dev.imbo.android.data.Access.Paid -> stringResource(R.string.subscribed)
        else -> null
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(ImboDimen.BrandBar)
            .padding(horizontal = ImboDimen.Gutter),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Wordmark()
        Spacer(Modifier.width(12.dp))
        if (status != null) {
            Text(
                text = status,
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                maxLines = 1,
                modifier = Modifier.weight(1f),
            )
        } else {
            Spacer(Modifier.weight(1f))
        }
        GhostButton(text = stringResource(R.string.subscribe), onClick = onSubscribe, height = 24)
        Spacer(Modifier.width(8.dp))
        Box(modifier = Modifier.size(ImboDimen.TouchTarget), contentAlignment = Alignment.Center) {
            Box(
                modifier = Modifier
                    .size(32.dp)
                    .clip(CircleShape)
                    .background(ImboColor.SecondaryContainer)
                    .androidxClickable(onAccount),
                contentAlignment = Alignment.Center,
            ) {
                Text("A", color = ImboColor.OnSecondaryContainer, fontFamily = ImboMono, fontSize = 12.sp)
            }
        }
    }
}

internal fun Modifier.androidxClickable(onClick: () -> Unit): Modifier = this.clickable(onClick = onClick)

@Composable
fun ImboTabs(
    active: String,
    chatsUnread: Int,
    sessionsUnread: Int,
    onSelect: (String) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(ImboDimen.TabRow)
            .padding(horizontal = 8.dp),
    ) {
        TabItem(stringResource(R.string.tab_chats), chatsUnread, active == "chats") { onSelect("chats") }
        TabItem(stringResource(R.string.tab_sessions), sessionsUnread, active == "sessions") { onSelect("sessions") }
        TabItem(stringResource(R.string.tab_contacts), 0, active == "contacts") { onSelect("contacts") }
    }
    Hairline(vertical = false)
}

@Composable
private fun RowScope.TabItem(label: String, unread: Int, selected: Boolean, onClick: () -> Unit) {
    Box(
        modifier = Modifier
            .weight(1f)
            .height(ImboDimen.TabRow)
            .androidxClickable(onClick),
        contentAlignment = Alignment.Center,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(
                text = label,
                color = if (selected) ImboColor.OnBackground else ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                fontWeight = FontWeight.Medium,
            )
            if (unread > 0) UnreadBadge(unread, small = true)
        }
        if (selected) {
            Box(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .fillMaxWidth(0.55f)
                    .height(2.dp)
                    .clip(RoundedCornerShape(1.dp))
                    .background(ImboColor.Primary),
            )
        }
    }
}

@Composable
fun ConversationRow(conversation: Conversation, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .androidxClickable(onClick)
            .padding(horizontal = 8.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Avatar(
            letter = conversation.title.take(1).uppercase(),
            group = conversation.isGroup,
        )
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = conversation.title,
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = conversation.lastMessage?.let { previewFor(conversation, it.preview) }.orEmpty(),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.width(12.dp))
        Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                text = relativeTime(conversation.lastMessage?.createdAt),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
            )
            if (conversation.pending > 0) UnreadBadge(conversation.pending) else Spacer(Modifier.height(20.dp))
        }
    }
}

private fun previewFor(conversation: Conversation, preview: String): String {
    // 群列表行显示「发送者的工具名 : 摘要」，单聊显示「对方的工具名 : 摘要」
    val handle = if (conversation.isGroup) conversation.lastMessage?.fromName else conversation.peer?.subtitle
    return if (handle.isNullOrBlank()) preview else "$handle : $preview"
}

@Composable
fun SectionLabel(text: String, action: (@Composable () -> Unit)? = null) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = ImboDimen.Gutter, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = text,
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 13.sp,
            fontWeight = FontWeight.Medium,
            modifier = Modifier.weight(1f),
        )
        action?.invoke()
    }
}

@Composable
fun PrimaryButton(text: String, onClick: () -> Unit, enabled: Boolean = true, modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .height(ImboDimen.Button)
            .clip(RoundedCornerShape(6.dp))
            .background(if (enabled) ImboColor.Primary else ImboColor.SecondaryContainer)
            .androidxClickable { if (enabled) onClick() },
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = text,
            color = if (enabled) ImboColor.OnPrimary else ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold,
        )
    }
}

@Composable
fun GhostButton(text: String, onClick: () -> Unit, height: Int = 24, enabled: Boolean = true) {
    Box(
        modifier = Modifier
            .height(height.dp)
            .clip(RoundedCornerShape(6.dp))
            .border(1.dp, if (enabled) ImboColor.Outline else ImboColor.OutlineVariant, RoundedCornerShape(6.dp))
            .androidxClickable { if (enabled) onClick() }
            .padding(horizontal = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text,
            color = if (enabled) ImboColor.OnBackground else ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
            fontWeight = FontWeight.Medium,
        )
    }
}

/**
 * 聊天页头部。agent 会话始终带「AI 会话」标识，不随滚动或消息状态消失（GAI-003）。
 */
@Composable
fun ChatHeader(
    title: String,
    subtitle: String,
    onBack: () -> Unit,
    showAvatar: Boolean,
    group: Boolean,
    agent: Boolean = false,
    /** 群头像地址：有图就用图，没有退回首字母/群图标。 */
    avatarUrl: String? = null,
    onSearch: () -> Unit = {},
    onMore: () -> Unit = {},
) {
    Column {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .height(ImboDimen.BrandBar)
                .padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton32(onClick = onBack) {
                Icon(Icons.Outlined.ChevronLeft, contentDescription = stringResource(R.string.back), tint = ImboColor.Tertiary)
            }
            if (showAvatar) {
                AvatarImage(letter = title.take(1).uppercase(), url = avatarUrl, group = group)
                Spacer(Modifier.width(8.dp))
            }
            Column(modifier = Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = title,
                        color = ImboColor.OnBackground,
                        fontFamily = ImboMono,
                        fontSize = 14.sp,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (agent) {
                        Spacer(Modifier.width(6.dp))
                        Box(
                            modifier = Modifier
                                .clip(RoundedCornerShape(4.dp))
                                .background(ImboColor.TertiaryContainer)
                                .border(1.dp, ImboColor.OutlineVariant, RoundedCornerShape(4.dp))
                                .padding(horizontal = 4.dp, vertical = 1.dp),
                        ) {
                            Text(
                                text = stringResource(R.string.ai_agent),
                                color = ImboColor.Tertiary,
                                fontFamily = ImboMono,
                                fontSize = 10.sp,
                            )
                        }
                    }
                }
                Text(
                    text = subtitle,
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            IconButton32(onClick = onSearch) {
                Icon(
                    Icons.Filled.Search,
                    contentDescription = stringResource(R.string.search),
                    tint = ImboColor.Tertiary,
                    modifier = Modifier.size(16.dp),
                )
            }
            IconButton32(onClick = onMore) {
                Icon(Icons.Outlined.MoreHoriz, contentDescription = stringResource(R.string.chat_more), tint = ImboColor.Tertiary)
            }
        }
        Hairline(vertical = false)
    }
}

@Composable
fun IconButton32(onClick: () -> Unit, content: @Composable () -> Unit) {
    Box(modifier = Modifier.size(32.dp).clip(RoundedCornerShape(6.dp)).androidxClickable(onClick), contentAlignment = Alignment.Center) {
        content()
    }
}

/** 单选圆点：给「语言」这类列表一个明确的可选标记，未选中也能看见。 */
@Composable
fun SelectionDot(selected: Boolean) {
    Box(modifier = Modifier.size(18.dp), contentAlignment = Alignment.Center) {
        Box(
            modifier = Modifier
                .size(18.dp)
                .clip(CircleShape)
                .border(1.dp, if (selected) ImboColor.OnBackground else ImboColor.Outline, CircleShape),
        )
        if (selected) {
            Box(
                modifier = Modifier
                    .size(8.dp)
                    .clip(CircleShape)
                    .background(ImboColor.OnBackground),
            )
        }
    }
}

@Composable
fun DayDivider(label: String) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = ImboDimen.Gutter, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Hairline(vertical = false, modifier = Modifier.weight(1f))
        Text(
            text = label,
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 12.sp,
            modifier = Modifier.padding(horizontal = 12.dp),
        )
        Hairline(vertical = false, modifier = Modifier.weight(1f))
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun MessageRow(
    message: Message,
    group: Boolean,
    selected: Boolean = false,
    onClick: () -> Unit = {},
    onLongPress: (Message) -> Unit = {},
    onOpenAttachment: (Attachment) -> Unit = {},
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            // 多选时给整行加一层底色，一眼看出哪几条被选中
            .background(if (selected) ImboColor.SecondaryContainer else Color.Transparent)
            .padding(horizontal = ImboDimen.Gutter, vertical = 6.dp),
        horizontalArrangement = if (message.mine) Arrangement.End else Arrangement.Start,
        verticalAlignment = Alignment.Bottom,
    ) {
        if (!message.mine) {
            Avatar(letter = message.senderName.take(1).uppercase())
            Spacer(Modifier.width(8.dp))
        }
        Column(
            horizontalAlignment = if (message.mine) Alignment.End else Alignment.Start,
        ) {
            // 发送失败：给一行明确提示，点气泡即重发
            if (message.delivery == Delivery.Failed) {
                Text(
                    text = stringResource(R.string.chat_failed_tap_retry),
                    color = ImboColor.Error,
                    fontFamily = ImboMono,
                    fontSize = 11.sp,
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    // 自己的消息统一显示「我」，别一半是账号名一半是 me
                    text = if (message.mine) stringResource(R.string.message_mine) else message.senderName,
                    color = if (message.mine) ImboColor.OnBackground else ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 14.sp,
                    fontWeight = if (message.mine) FontWeight.SemiBold else FontWeight.Normal,
                )
                Hairline()
                Text(
                    text = clockTime(message.createdAt),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                )
                // 从哪发的（电脑终端 / 网页 / App）：排查「这句是谁说的」时有用
                message.via?.let { via ->
                    Hairline()
                    Text(
                        text = viaLabel(via),
                        color = ImboColor.Tertiary,
                        fontFamily = ImboMono,
                        fontSize = 11.sp,
                    )
                }
                // 发给自己会话的消息：还没被会话领取时讲清楚，别看起来像已送达
                if (message.serverState == "pending" || message.serverState == "pushed" || message.serverState == "canceled") {
                    Hairline()
                    Text(
                        text = if (message.serverState == "canceled") {
                            stringResource(R.string.message_state_canceled)
                        } else {
                            stringResource(R.string.message_state_waiting)
                        },
                        color = ImboColor.Tertiary,
                        fontFamily = ImboMono,
                        fontSize = 11.sp,
                    )
                }
            }
            Spacer(Modifier.height(4.dp))
            Box(
                modifier = Modifier
                    .clip(RoundedCornerShape(ImboDimen.BubbleRadius))
                    .background(if (message.mine) ImboColor.Primary else ImboColor.SecondaryContainer)
                    .combinedClickable(
                        onClick = onClick,
                        onLongClick = { onLongPress(message) },
                    )
                    .padding(horizontal = 14.dp, vertical = 10.dp),
            ) {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    message.attachments.forEach { attachment ->
                        AttachmentTile(attachment = attachment, mine = message.mine, onOpen = onOpenAttachment)
                    }
                    if (message.body.isNotBlank()) {
                        Text(
                            text = mentionStyled(message.body),
                            color = if (message.mine) ImboColor.OnPrimary else ImboColor.OnSecondaryContainer,
                            fontFamily = ImboMono,
                            fontSize = 14.sp,
                            lineHeight = 22.sp,
                        )
                    }
                }
            }
            if (message.delivery == dev.imbo.android.data.Delivery.Sending) {
                Text(
                    text = "Sending…",
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
        }
    }
}

/**
 * 气泡里的一个附件：有本地 uri 的图片用 Coil 直接显示（content:// 也支持），
 * 其余情况（收到的文件、本地非图片）显示文件名和大小。
 */
@Composable
fun AttachmentTile(attachment: Attachment, mine: Boolean, onOpen: (Attachment) -> Unit = {}) {
    val contentColor = if (mine) ImboColor.OnPrimary else ImboColor.OnSecondaryContainer
    if (attachment.mimeType.startsWith("image/") && attachment.localUri != null) {
        AsyncImage(
            model = attachment.localUri,
            contentDescription = attachment.name,
            contentScale = ContentScale.Crop,
            modifier = Modifier
                .width(200.dp)
                .height(150.dp)
                .clip(RoundedCornerShape(6.dp))
                .background(ImboColor.SecondaryContainer)
                // 点图片看大图：气泡里的缩略图只有 200×150，细节看不清
                .clickable { onOpen(attachment) },
        )
    } else {
        Row(
            modifier = Modifier
                .widthIn(max = 240.dp)
                .clip(RoundedCornerShape(6.dp))
                .border(1.dp, ImboColor.OutlineVariant, RoundedCornerShape(6.dp))
                .padding(horizontal = 10.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                Icons.AutoMirrored.Outlined.InsertDriveFile,
                contentDescription = null,
                tint = contentColor,
                modifier = Modifier.size(20.dp),
            )
            Spacer(Modifier.width(8.dp))
            Column {
                Text(
                    text = attachment.name,
                    color = contentColor,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                val size = formatFileSize(attachment.sizeBytes)
                if (size.isNotEmpty()) {
                    Text(
                        text = size,
                        color = contentColor.copy(alpha = 0.65f),
                        fontFamily = ImboMono,
                        fontSize = 11.sp,
                    )
                }
            }
        }
    }
}

/** 文件大小按 1024 进制显示；未知大小返回空串，不占一行。 */
fun formatFileSize(bytes: Long): String {
    if (bytes <= 0) return ""
    val locale = java.util.Locale.getDefault()
    return when {
        bytes < 1024 -> "$bytes B"
        bytes < 1024 * 1024 -> String.format(locale, "%.1f KB", bytes / 1024.0)
        bytes < 1024L * 1024 * 1024 -> String.format(locale, "%.1f MB", bytes / (1024.0 * 1024.0))
        else -> String.format(locale, "%.1f GB", bytes / (1024.0 * 1024.0 * 1024.0))
    }
}

@Composable
fun Composer(
    value: String,
    onValueChange: (String) -> Unit,
    onSend: () -> Unit,
    onAttach: () -> Unit = {},
) {
    Column {
        Hairline(vertical = false)
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = ImboDimen.Gutter, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Box(
                modifier = Modifier
                    .size(ImboDimen.Field)
                    .clip(RoundedCornerShape(6.dp))
                    .border(1.dp, ImboColor.Outline, RoundedCornerShape(6.dp))
                    .androidxClickable(onAttach),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Filled.Add,
                    contentDescription = stringResource(R.string.attach),
                    tint = ImboColor.Tertiary,
                    modifier = Modifier.size(18.dp),
                )
            }
            androidx.compose.foundation.text.BasicTextField(
                value = value,
                onValueChange = onValueChange,
                singleLine = true,
                textStyle = androidx.compose.ui.text.TextStyle(
                    color = ImboColor.OnBackground,
                    fontFamily = ImboMono,
                    fontSize = 14.sp,
                ),
                cursorBrush = androidx.compose.ui.graphics.SolidColor(ImboColor.Primary),
                modifier = Modifier
                    .weight(1f)
                    .height(ImboDimen.Field)
                    .clip(RoundedCornerShape(6.dp))
                    .border(1.dp, ImboColor.Outline, RoundedCornerShape(6.dp))
                    .background(ImboColor.TertiaryContainer)
                    .padding(horizontal = 12.dp),
                decorationBox = { inner ->
                    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.CenterStart) {
                        if (value.isEmpty()) {
                            Text(
                                text = stringResource(R.string.message_placeholder),
                                color = ImboColor.Tertiary,
                                fontFamily = ImboMono,
                                fontSize = 14.sp,
                            )
                        }
                        inner()
                    }
                },
            )
            Box(
                modifier = Modifier
                    .size(ImboDimen.Field)
                    .clip(RoundedCornerShape(6.dp))
                    .background(ImboColor.Primary)
                    .androidxClickable(onSend),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Filled.ArrowUpward,
                    contentDescription = stringResource(R.string.send),
                    tint = ImboColor.OnPrimary,
                    modifier = Modifier.size(18.dp),
                )
            }
        }
    }
}

@Composable
fun EmptyState(title: String, body: String) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 32.dp, vertical = 48.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(title, color = ImboColor.OnBackground, fontFamily = ImboMono, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(8.dp))
        Text(body, color = ImboColor.Tertiary, fontFamily = ImboMono, fontSize = 12.sp)
    }
}

/** 时间显示：当天 HH:mm，本周 EEE，更早 MMM d。 */
fun clockTime(epochMillis: Long): String {
    val zone = java.time.ZoneId.systemDefault()
    val time = java.time.Instant.ofEpochMilli(epochMillis).atZone(zone)
    return String.format("%02d:%02d", time.hour, time.minute)
}

/** 消息来源的短标签；未知值原样显示，免得多一种来源就丢信息。 */@Composable
private fun viaLabel(via: String): String = when (via) {
    "app" -> stringResource(R.string.via_app)
    "web" -> stringResource(R.string.via_web)
    "terminal" -> stringResource(R.string.via_terminal)
    else -> via
}

/**
 * 群消息里的 @提及 高亮：只把「@名字」加粗，其余原样。
 * 纯显示处理，发送内容不变——服务端是按正文里的「@名字」判断 mention 的。
 */
fun mentionStyled(body: String): AnnotatedString = buildAnnotatedString {
    append(body)
    Regex("@[^\\s@]+").findAll(body).forEach { m ->
        addStyle(SpanStyle(fontWeight = FontWeight.Bold), m.range.first, m.range.last + 1)
    }
}

fun relativeTime(epochMillis: Long?): String {
    if (epochMillis == null) return ""
    val zone = java.time.ZoneId.systemDefault()
    val time = java.time.Instant.ofEpochMilli(epochMillis).atZone(zone)
    val now = java.time.ZonedDateTime.now(zone)
    return when {
        time.toLocalDate() == now.toLocalDate() -> String.format("%02d:%02d", time.hour, time.minute)
        time.toLocalDate() == now.toLocalDate().minusDays(1) -> localizedWord("昨天", "Yesterday")
        time.isAfter(now.minusDays(7)) -> time.dayOfWeek.getDisplayName(java.time.format.TextStyle.SHORT, java.util.Locale.getDefault())
        else -> "${time.month.getDisplayName(java.time.format.TextStyle.SHORT, java.util.Locale.getDefault())} ${time.dayOfMonth}"
    }
}

/**
 * 这两个时间标签不是从 Compose 里取的（调用点有非 @Composable 的 lambda），
 * 所以按当前 Locale 判断语言——应用内切换语言时 LocaleStore 已经把默认 Locale 换掉了。
 */
private fun localizedWord(zh: String, en: String): String =
    if (java.util.Locale.getDefault().language.startsWith("zh")) zh else en

fun dayLabel(epochMillis: Long): String {
    val zone = java.time.ZoneId.systemDefault()
    val time = java.time.Instant.ofEpochMilli(epochMillis).atZone(zone)
    val now = java.time.ZonedDateTime.now(zone)
    return when (time.toLocalDate()) {
        now.toLocalDate() -> localizedWord("今天", "Today")
        now.toLocalDate().minusDays(1) -> localizedWord("昨天", "Yesterday")
        else -> time.dayOfWeek.getDisplayName(java.time.format.TextStyle.FULL, java.util.Locale.getDefault())
    }
}
/** 全屏看图：点一下任意位置关闭，图片按比例缩放不裁剪。 */
@Composable
fun ImageViewer(attachment: Attachment, onDismiss: () -> Unit) {
    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black)
                .clickable { onDismiss() },
            contentAlignment = Alignment.Center,
        ) {
            AsyncImage(
                model = attachment.localUri,
                contentDescription = attachment.name,
                contentScale = ContentScale.Fit,
                modifier = Modifier.fillMaxSize().padding(8.dp),
            )
        }
    }
}
/** 打开会话时，第一条未读消息上方的分隔线。 */
@Composable
fun UnreadDivider() {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = ImboDimen.Gutter, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.weight(1f).height(1.dp).background(ImboColor.OutlineVariant))
        Text(
            text = stringResource(R.string.chat_unread_divider),
            color = ImboColor.Tertiary,
            fontFamily = ImboMono,
            fontSize = 11.sp,
            modifier = Modifier.padding(horizontal = 8.dp),
        )
        Box(Modifier.weight(1f).height(1.dp).background(ImboColor.OutlineVariant))
    }
}