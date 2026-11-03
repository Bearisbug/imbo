package dev.imbo.android.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import dev.imbo.android.R
import dev.imbo.android.ui.theme.ImboColor
import dev.imbo.android.ui.theme.ImboMono

/** 统一的确认对话框：深色底、等宽字体，破坏性操作用 error 色。 */
@Composable
fun ImboDialog(
    title: String,
    body: String?,
    confirmLabel: String,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    dismissLabel: String = stringResource(R.string.cancel),
    destructive: Boolean = false,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = ImboColor.SecondaryContainer,
        shape = RoundedCornerShape(8.dp),
        title = {
            Text(
                text = title,
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
            )
        },
        text = body?.let {
            {
                Text(
                    text = it,
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                    lineHeight = 20.sp,
                )
            }
        },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(
                    text = confirmLabel,
                    color = if (destructive) ImboColor.Error else ImboColor.OnBackground,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.Medium,
                )
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(dismissLabel, color = ImboColor.Tertiary, fontFamily = ImboMono, fontSize = 13.sp)
            }
        },
    )
}

/** 单字段编辑对话框，用于改名和改邮箱。 */
@Composable
fun ImboTextFieldDialog(
    title: String,
    label: String,
    initial: String,
    confirmLabel: String,
    onConfirm: (String) -> Unit,
    onDismiss: () -> Unit,
    hint: String? = null,
) {
    var value by remember { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = ImboColor.SecondaryContainer,
        shape = RoundedCornerShape(8.dp),
        title = {
            Text(
                text = title,
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
            )
        },
        text = {
            Column(modifier = Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                InsetField(
                    label = label,
                    value = value,
                    placeholder = initial,
                    onValueChange = { value = it },
                    hint = hint,
                )
                Spacer(Modifier.height(0.dp))
            }
        },
        confirmButton = {
            TextButton(onClick = { onConfirm(value) }, enabled = value.isNotBlank()) {
                Text(
                    text = confirmLabel,
                    color = if (value.isNotBlank()) ImboColor.OnBackground else ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.Medium,
                )
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.cancel), color = ImboColor.Tertiary, fontFamily = ImboMono, fontSize = 13.sp)
            }
        },
    )
}

/** 举报原因选择：列表式，一步选中即提交。 */
@Composable
fun ReportDialog(
    onReport: (String) -> Unit,
    onReportAndBlock: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val reasons = listOf(
        stringResource(R.string.report_reason_spam),
        stringResource(R.string.report_reason_harassment),
        stringResource(R.string.report_reason_other),
    )
    var selected by remember { mutableStateOf(reasons.first()) }
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = ImboColor.SecondaryContainer,
        shape = RoundedCornerShape(8.dp),
        title = {
            Text(
                text = stringResource(R.string.report_title),
                color = ImboColor.OnBackground,
                fontFamily = ImboMono,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
            )
        },
        text = {
            Column(modifier = Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    text = stringResource(R.string.report_body),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    lineHeight = 18.sp,
                )
                reasons.forEach { reason ->
                    val active = reason == selected
                    Text(
                        text = if (active) "✓  $reason" else "    $reason",
                        color = if (active) ImboColor.OnBackground else ImboColor.Tertiary,
                        fontFamily = ImboMono,
                        fontSize = 13.sp,
                        modifier = Modifier
                            .fillMaxWidth()
                            .background(if (active) ImboColor.TertiaryContainer else ImboColor.SecondaryContainer)
                            .padding(vertical = 8.dp, horizontal = 4.dp)
                            .androidxClickable { selected = reason },
                    )
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onReportAndBlock(selected) }) {
                Text(
                    text = stringResource(R.string.report_block_after),
                    color = ImboColor.Error,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.Medium,
                )
            }
        },
        dismissButton = {
            TextButton(onClick = { onReport(selected) }) {
                Text(
                    text = stringResource(R.string.report),
                    color = ImboColor.OnBackground,
                    fontFamily = ImboMono,
                    fontSize = 13.sp,
                )
            }
        },
    )
}

/** 会话「更多」动作列表：举报固定在这一层，AI 会话还会先说明对方是 AI。 */
@Composable
fun ChatActionsDialog(
    agentName: String?,
    onReport: () -> Unit,
    onBlock: () -> Unit,
    onDismiss: () -> Unit,
    onGroupInfo: (() -> Unit)? = null,
) {
    Dialog(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .background(ImboColor.SecondaryContainer)
                .padding(vertical = 4.dp),
        ) {
            if (agentName != null) {
                Text(
                    text = stringResource(R.string.ai_agent_hint, agentName),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    lineHeight = 18.sp,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
                )
                Hairline(vertical = false)
            }
            if (onGroupInfo != null) {
                ActionRow(stringResource(R.string.group_info), ImboColor.OnBackground, onGroupInfo)
            }
            ActionRow(stringResource(R.string.report), ImboColor.OnBackground, onReport)
            ActionRow(stringResource(R.string.block_action), ImboColor.Error, onBlock)
            Hairline(vertical = false)
            ActionRow(stringResource(R.string.cancel), ImboColor.Tertiary, onDismiss)
        }
    }
}

/** 长按消息气泡弹出的动作：复制、引用、多选、取消；服务端没有删除消息的接口，所以不提供删除。 */
@Composable
fun MessageActionsDialog(
    onCopy: () -> Unit,
    onQuote: () -> Unit,
    onDismiss: () -> Unit,
    onSelect: (() -> Unit)? = null,
    onForward: (() -> Unit)? = null,
) {
    Dialog(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .background(ImboColor.SecondaryContainer)
                .padding(vertical = 4.dp),
        ) {
            ActionRow(stringResource(R.string.chat_copy), ImboColor.OnBackground, onCopy)
            ActionRow(stringResource(R.string.chat_quote), ImboColor.OnBackground, onQuote)
            if (onForward != null) {
                ActionRow(stringResource(R.string.chat_forward), ImboColor.OnBackground, onForward)
            }
            if (onSelect != null) {
                ActionRow(stringResource(R.string.chat_select), ImboColor.OnBackground, onSelect)
            }
            Hairline(vertical = false)
            ActionRow(stringResource(R.string.cancel), ImboColor.Tertiary, onDismiss)
        }
    }
}

/** 输入栏「+」打开的选择面板：拍照、照片选择器、文档选择器。 */
@Composable
fun AttachmentPickerDialog(
    onCamera: () -> Unit = {},
    onPhoto: () -> Unit,
    onFile: () -> Unit,
    onDismiss: () -> Unit,
) {
    Dialog(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .background(ImboColor.SecondaryContainer)
                .padding(vertical = 4.dp),
        ) {
            Text(
                text = stringResource(R.string.attach_picker_title),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
            )
            Hairline(vertical = false)
            ActionRow(stringResource(R.string.attach_camera), ImboColor.OnBackground, onCamera)
            ActionRow(stringResource(R.string.attach_photo), ImboColor.OnBackground, onPhoto)
            ActionRow(stringResource(R.string.attach_file), ImboColor.OnBackground, onFile)
            Hairline(vertical = false)
            ActionRow(stringResource(R.string.cancel), ImboColor.Tertiary, onDismiss)
        }
    }
}

@Composable
private fun ActionRow(text: String, color: androidx.compose.ui.graphics.Color, onClick: () -> Unit) {
    Text(
        text = text,
        color = color,
        fontFamily = ImboMono,
        fontSize = 14.sp,
        modifier = Modifier
            .fillMaxWidth()
            .androidxClickable(onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
    )
}
/** 输入 @ 之后的成员选择：点一个人就把他填进输入框。 */
@Composable
fun MentionPickerDialog(
    members: List<dev.imbo.android.data.Contact>,
    onPick: (dev.imbo.android.data.Contact) -> Unit,
    onDismiss: () -> Unit,
) {
    Dialog(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .background(ImboColor.SecondaryContainer)
                .padding(vertical = 4.dp),
        ) {
            Text(
                text = stringResource(R.string.mention_picker_title),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
            )
            Hairline(vertical = false)
            members.take(8).forEach { member ->
                androidx.compose.foundation.layout.Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { onPick(member) }
                        .padding(horizontal = 16.dp, vertical = 10.dp),
                    verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                ) {
                    Avatar(letter = member.name.take(1).uppercase())
                    Spacer(Modifier.width(12.dp))
                    Text(
                        text = "@" + member.name,
                        color = ImboColor.OnBackground,
                        fontFamily = ImboMono,
                        fontSize = 14.sp,
                    )
                }
            }
            Hairline(vertical = false)
            ActionRow(stringResource(R.string.cancel), ImboColor.Tertiary, onDismiss)
        }
    }
}
/** 转发目标选择：列出除当前会话外的全部对话，点一条即转发。 */
@Composable
fun ForwardDialog(
    targets: List<dev.imbo.android.data.Conversation>,
    onPick: (Long) -> Unit,
    onDismiss: () -> Unit,
) {
    Dialog(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .background(ImboColor.SecondaryContainer)
                .padding(vertical = 4.dp),
        ) {
            Text(
                text = stringResource(R.string.forward_title),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
            )
            Hairline(vertical = false)
            if (targets.isEmpty()) {
                Text(
                    text = stringResource(R.string.forward_empty),
                    color = ImboColor.Tertiary,
                    fontFamily = ImboMono,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
                )
            }
            targets.take(8).forEach { conversation ->
                androidx.compose.foundation.layout.Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { onPick(conversation.id) }
                        .padding(horizontal = 16.dp, vertical = 10.dp),
                    verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                ) {
                    Avatar(
                        letter = conversation.title.take(1).uppercase(),
                        group = conversation.isGroup,
                    )
                    Spacer(Modifier.width(12.dp))
                    Text(
                        text = conversation.title.ifBlank { stringResource(R.string.chat_empty_title) },
                        color = ImboColor.OnBackground,
                        fontFamily = ImboMono,
                        fontSize = 14.sp,
                    )
                }
            }
            Hairline(vertical = false)
            ActionRow(stringResource(R.string.cancel), ImboColor.Tertiary, onDismiss)
        }
    }
}
/** 联系人会话共享：全部 / 仅选中 / 不共享；选「仅选中」时列出我的会话。 */
@Composable
fun ContactShareDialog(
    share: dev.imbo.android.data.ContactShare,
    onMode: (String) -> Unit,
    onToggleSession: (String, Boolean) -> Unit,
    onDismiss: () -> Unit,
) {
    val chosen = share.share == "chosen"
    Dialog(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .background(ImboColor.SecondaryContainer)
                .padding(vertical = 4.dp),
        ) {
            Text(
                text = stringResource(R.string.share_title),
                color = ImboColor.Tertiary,
                fontFamily = ImboMono,
                fontSize = 12.sp,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
            )
            Hairline(vertical = false)
            listOf("all" to R.string.share_all, "chosen" to R.string.share_chosen, "none" to R.string.share_none)
                .forEach { (mode, label) ->
                    ActionRow(
                        text = stringResource(label),
                        color = if (share.share == mode) ImboColor.OnBackground else ImboColor.Tertiary,
                        onClick = { onMode(mode) },
                    )
                }
            if (chosen) {
                share.sessions.forEach { session ->
                    ActionRow(
                        text = (if (session.selected) "✓ " else "　") + session.title,
                        color = if (session.selected) ImboColor.OnBackground else ImboColor.Tertiary,
                        onClick = { onToggleSession(session.id, !session.selected) },
                    )
                }
            }
            Hairline(vertical = false)
            ActionRow(stringResource(R.string.cancel), ImboColor.Tertiary, onDismiss)
        }
    }
}
