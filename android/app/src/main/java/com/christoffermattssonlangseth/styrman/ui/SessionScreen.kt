package com.christoffermattssonlangseth.styrman.ui

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.material.icons.filled.Difference
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.RestartAlt
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.StopCircle
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material.icons.filled.VisibilityOff
import androidx.compose.material.icons.filled.VolumeOff
import androidx.compose.material.icons.filled.VolumeUp
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.RadioButton
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.AgentKind
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.Dictation
import com.christoffermattssonlangseth.styrman.FileDiff
import com.christoffermattssonlangseth.styrman.PermissionMode
import com.christoffermattssonlangseth.styrman.SessionSummary
import com.christoffermattssonlangseth.styrman.SessionStatus
import com.christoffermattssonlangseth.styrman.SessionTimeline
import com.christoffermattssonlangseth.styrman.Speaker
import com.christoffermattssonlangseth.styrman.TimelineItem
import com.christoffermattssonlangseth.styrman.ToolKind
import com.christoffermattssonlangseth.styrman.ToolStatus
import kotlinx.coroutines.delay

/** How a tool call reads: the bare command for shell tools, paths relative to the folder. */
fun displayTitle(tool: com.christoffermattssonlangseth.styrman.ToolInfo, cwd: String?): String {
    if (tool.kind == ToolKind.Execute) {
        (tool.input as? kotlinx.serialization.json.JsonObject)?.get("command")
            ?.let { it as? kotlinx.serialization.json.JsonPrimitive }?.takeIf { it.isString }?.let { return it.content }
    }
    if (cwd.isNullOrEmpty()) return tool.title
    return tool.title.replace(if (cwd.endsWith("/")) cwd else "$cwd/", "")
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionScreen(connection: DaemonConnection, speaker: Speaker, sessionId: String, onBack: () -> Unit) {
    val timeline = connection.timeline(sessionId)
    val summary = connection.session(sessionId)
    val agent = summary?.agent ?: AgentKind.Unknown
    val list = rememberLazyListState()
    // The newest reply once history has loaded; only later ones are read aloud.
    var baseline by remember { mutableStateOf<String?>(null) }
    var baselineSet by remember { mutableStateOf(false) }

    DisposableEffect(sessionId) {
        connection.watch(sessionId)
        onDispose { connection.unwatch(sessionId); speaker.stop() }
    }
    val latestReply = timeline.items.lastOrNull { it is TimelineItem.Assistant } as? TimelineItem.Assistant
    LaunchedEffect(timeline.isLive) {
        if (timeline.isLive && !baselineSet) { baseline = latestReply?.id; baselineSet = true }
    }
    LaunchedEffect(latestReply?.id) {
        val r = latestReply ?: return@LaunchedEffect
        if (baselineSet && r.id != baseline) {
            baseline = r.id
            if (speaker.autoRead) speaker.speak(r.text, r.id, agent)
        }
    }
    val preview = timeline.preview?.takeIf { summary?.status == SessionStatus.Working }
    LaunchedEffect(timeline.items.size + timeline.pending.size) {
        val n = timeline.items.size + timeline.pending.size
        if (n > 0) list.animateScrollToItem(n - 1)
    }
    // Follow the reply as it grows, unless you've scrolled up to read something.
    LaunchedEffect(preview?.length) {
        val info = list.layoutInfo
        if (preview != null && (info.visibleItemsInfo.lastOrNull()?.index ?: 0) >= info.totalItemsCount - 2) list.scrollBy(10_000f)
    }
    var menu by remember { mutableStateOf(false) }
    var confirmEnd by remember { mutableStateOf(false) }
    var pickMode by remember { mutableStateOf(false) }
    var showChanges by remember { mutableStateOf(false) }
    var showTerminal by remember { mutableStateOf(false) }

    Scaffold(
        topBar = {
            TopAppBar(
                // The bar takes the agent's colour, so it's always clear who you're talking to.
                colors = androidx.compose.material3.TopAppBarDefaults.topAppBarColors(
                    containerColor = agentColor(agent).copy(alpha = 0.07f)
                        .compositeOver(MaterialTheme.colorScheme.surface),
                ),
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (summary != null) { AgentAvatar(summary.agent, 26.dp); Spacer(Modifier.width(8.dp)) }
                        Column {
                            Text(summary?.repoName ?: "Session", maxLines = 1, overflow = TextOverflow.Ellipsis)
                            // Manual is the usual mode; only the others are worth the room.
                            summary?.permissionMode?.takeIf { it != PermissionMode.Manual }?.let { m ->
                                Text(
                                    "${m.short} mode", style = MaterialTheme.typography.labelSmall, maxLines = 1,
                                    color = if (m == PermissionMode.BypassPermissions) MaterialTheme.colorScheme.error
                                    else MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                },
                actions = {
                    IconButton(onClick = {
                        speaker.updateAutoRead(!speaker.autoRead)
                        if (speaker.autoRead) latestReply?.let { speaker.speak(it.text, it.id, agent) }
                    }) {
                        Icon(
                            if (speaker.autoRead) Icons.Filled.VolumeUp else Icons.Filled.VolumeOff,
                            if (speaker.autoRead) "Stop reading replies aloud" else "Read replies aloud",
                            tint = if (speaker.autoRead) agentColor(agent) else MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    summary?.let { s ->
                        StatusChip(s.status)
                        Box {
                            IconButton(onClick = { menu = true }) { Icon(Icons.Filled.MoreVert, "More") }
                            SessionMenu(
                                s, expanded = menu, onDismiss = { menu = false },
                                onMode = { pickMode = true },
                                onChanges = { showChanges = true },
                                onTerminal = { showTerminal = true },
                                onEnd = { confirmEnd = true },
                                onStopSharing = { connection.stopSharing(sessionId); onBack() },
                            )
                        }
                    }
                },
            )
        },
        bottomBar = {
            Surface(tonalElevation = 3.dp, modifier = Modifier.imePadding().navigationBarsPadding()) {
                Column {
                    if (!timeline.isLive && (summary != null || connection.state != DaemonConnection.State.Connected)) {
                        Notice(if (connection.state == DaemonConnection.State.Connected) "Catching up…" else "Disconnected from Mac")
                    }
                    when {
                        summary == null && connection.state == DaemonConnection.State.Connected ->
                            Notice("This session is no longer shared.")
                        summary == null -> {}
                        summary.acceptsPrompts -> Composer(connection, sessionId, summary.status, agent, canStop = summary.hasLiveControls)
                        else -> Notice(
                            "Read-only: this ${agent.displayName} session wasn't started through Styrman. " +
                                "Run `styr setup` on the Mac, then start it again in a new terminal tab.",
                        )
                    }
                }
            }
        },
    ) { pad ->
        LazyColumn(
            state = list,
            modifier = Modifier.fillMaxSize().padding(pad),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            items(timeline.items, key = { it.id }) { item ->
                TimelineRow(item, summary?.cwd, agent, speaker, timeline) { request, option ->
                    connection.resolvePermission(sessionId, request, option)
                }
            }
            items(timeline.pending, key = { it.id }) { p ->
                PendingRow(p) { timeline.dismissUnconfirmed(p.id) }
            }
            if (preview != null) item(key = "preview") { LivePreviewRow(preview, agent) }
            if (timeline.isLive && !timeline.hasContent && timeline.pending.isEmpty()) item {
                Notice("Nothing yet. Prompts, tool calls and replies show up here as they happen.")
            }
        }
    }
    if (confirmEnd) AlertDialog(
        onDismissRequest = { confirmEnd = false },
        title = { Text("End this session?") },
        text = {
            Text("${agent.displayName} quits on your Mac, as if you pressed Ctrl+C in its terminal. " +
                "If it was started from this phone, its cmux tab closes too.")
        },
        confirmButton = {
            TextButton(onClick = { confirmEnd = false; connection.endSession(sessionId); onBack() }) {
                Text("End session", color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = { TextButton(onClick = { confirmEnd = false }) { Text("Cancel") } },
    )
    summary?.permissionMode?.let { current ->
        if (pickMode) ModeDialog(current, onDismiss = { pickMode = false }) { connection.setMode(sessionId, it); pickMode = false }
    }
    if (showChanges) ChangesDialog(connection, sessionId, agent) { showChanges = false }
    if (showTerminal) TerminalDialog(connection, sessionId, agent) { showTerminal = false }
    connection.lastError?.let { msg ->
        LaunchedEffect(msg) { delay(3500); connection.lastError = null }
        AlertDialog(onDismissRequest = { connection.lastError = null }, confirmButton = {
            TextButton(onClick = { connection.lastError = null }) { Text("OK") }
        }, text = { Text(msg) })
    }
}

@Composable
private fun TimelineRow(
    item: TimelineItem,
    cwd: String?,
    agent: AgentKind,
    speaker: Speaker,
    timeline: SessionTimeline,
    answer: (String, String) -> Boolean,
) {
    when (item) {
        is TimelineItem.Divider -> Text(
            item.text, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.fillMaxWidth(), textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
        is TimelineItem.User -> agentMessage(item.text)?.let { m -> AgentMessageRow(m) } ?: Row(Modifier.fillMaxWidth()) {
            Spacer(Modifier.weight(1f).width(40.dp))
            Column(horizontalAlignment = Alignment.End) {
                SelectionContainer {
                    Text(item.text, Modifier.background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(16.dp)).padding(horizontal = 14.dp, vertical = 10.dp))
                }
                if (item.fromPhone) Text("From this phone", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        is TimelineItem.Assistant -> Reply(item, agent, speaker)
        is TimelineItem.Tool -> ToolRow(item, displayTitle(item.tool, cwd), cwd)
        is TimelineItem.Permission -> PermissionCard(item, displayTitle(item.tool, cwd), timeline.diffs(item), cwd, answer)
        is TimelineItem.Plan -> Column(
            Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainer, RoundedCornerShape(12.dp)).padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text("Plan", fontWeight = FontWeight.SemiBold)
            item.entries.forEach { e ->
                Text(
                    (if (e.status == "completed") "✓ " else if (e.status == "in_progress") "◐ " else "○ ") + e.content,
                    textDecoration = if (e.status == "completed") TextDecoration.LineThrough else null,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
        is TimelineItem.Notice -> Text(
            item.text, style = MaterialTheme.typography.bodySmall,
            color = if (item.warning) Color(0xFFF29A2E) else MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** A reply with copy and play/stop under it (like the Claude app). */
@Composable
private fun Reply(item: TimelineItem.Assistant, agent: AgentKind, speaker: Speaker) {
    val context = LocalContext.current
    val playing = speaker.speakingId == item.id
    val tint = agentColor(agent)
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) { if (copied) { delay(1500); copied = false } }
    // A thin edge in the agent's colour on every reply; solid while it's read aloud.
    Row(Modifier.fillMaxWidth().height(androidx.compose.foundation.layout.IntrinsicSize.Min)) {
        Box(Modifier.width(if (playing) 4.dp else 3.dp).fillMaxHeight().background(tint.copy(alpha = if (playing) 1f else 0.45f), RoundedCornerShape(2.dp)))
        Column(Modifier.padding(start = 12.dp)) {
            SelectionContainer { Text(markdown(item.text, MaterialTheme.colorScheme.surfaceContainerHighest)) }
            Row {
                IconButton(onClick = {
                    (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager)
                        .setPrimaryClip(ClipData.newPlainText("reply", item.text))
                    copied = true
                }) {
                    Icon(if (copied) Icons.Filled.Check else Icons.Filled.ContentCopy, if (copied) "Copied" else "Copy",
                        tint = if (copied) tint else MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(18.dp))
                }
                IconButton(onClick = { speaker.toggle(item.text, item.id, agent) }) {
                    Icon(if (playing) Icons.Filled.Stop else Icons.Filled.VolumeUp, if (playing) "Stop reading" else "Read aloud",
                        tint = if (playing) tint else MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(18.dp))
                }
            }
        }
    }
}

@Composable
private fun ToolRow(item: TimelineItem.Tool, title: String, cwd: String?) {
    var open by remember { mutableStateOf(false) }
    val diffs = remember(item.tool) { if (item.tool.kind == ToolKind.Edit) FileDiff.from(item.tool) else null }
    Row(
        Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainer, RoundedCornerShape(8.dp))
            .clickable { open = true }.padding(horizontal = 10.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        // Compact: one line, small type. Tap for the full command and output.
        Icon(toolIcon(item.tool.kind), null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(14.dp))
        Text(title, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis,
            fontFamily = if (item.tool.kind == ToolKind.Execute) Mono else null,
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        diffs?.let { DiffCounts(it) }
        when (item.status) {
            ToolStatus.Pending, ToolStatus.Running -> CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
            ToolStatus.Completed -> Icon(Icons.Filled.Check, "Done", tint = Color(0xFF34A853), modifier = Modifier.size(16.dp))
            ToolStatus.Failed -> Icon(Icons.Filled.Close, "Failed", tint = Color(0xFFD93025), modifier = Modifier.size(16.dp))
        }
    }
    if (open) AlertDialog(
        onDismissRequest = { open = false },
        confirmButton = { TextButton(onClick = { open = false }) { Text("Done") } },
        title = { Text(item.tool.name) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                SelectionContainer { Text(title, fontFamily = Mono) }
                diffs?.let {
                    Spacer(Modifier.size(12.dp))
                    Text("Changes", fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.size(6.dp))
                    DiffPreview(it, cwd)
                }
                Spacer(Modifier.size(12.dp))
                Text("Output", fontWeight = FontWeight.SemiBold)
                SelectionContainer {
                    Text(
                        item.output?.ifEmpty { "(no output)" }
                            ?: if (item.status == ToolStatus.Completed || item.status == ToolStatus.Failed) "(no output)" else "Still running…",
                        fontFamily = Mono, style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.horizontalScroll(rememberScrollState()),
                    )
                }
            }
        },
    )
}

@OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
private fun PermissionCard(
    item: TimelineItem.Permission,
    title: String,
    diffs: List<FileDiff>?,
    cwd: String?,
    answer: (String, String) -> Boolean,
) {
    var sent by remember(item.request) { mutableStateOf<String?>(null) }
    val attention = attentionColor()
    Column(
        Modifier.fillMaxWidth()
            .background(if (item.isOpen) attention.copy(alpha = 0.08f) else Color.Transparent, RoundedCornerShape(14.dp))
            .border(1.dp, if (item.isOpen) attention.copy(alpha = 0.6f) else MaterialTheme.colorScheme.outlineVariant, RoundedCornerShape(14.dp))
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Icon(Icons.Filled.PanTool, null, tint = if (item.isOpen) attention else MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(16.dp))
            Text(if (item.isOpen) "Permission needed" else "Permission", fontWeight = FontWeight.SemiBold,
                color = if (item.isOpen) attention else MaterialTheme.colorScheme.onSurfaceVariant)
        }
        // Edits as a colored diff; anything else as its title.
        if (diffs != null) DiffPreview(diffs, cwd, maxLines = if (item.isOpen) 24 else 8)
        else SelectionContainer { Text(title, fontFamily = if (item.tool.kind == ToolKind.Execute) Mono else null) }
        item.reason?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        if (item.isOpen) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                item.options.forEach { opt ->
                    Button(
                        onClick = { if (answer(item.request, opt.id)) sent = opt.id },
                        enabled = sent == null,
                        colors = ButtonDefaults.buttonColors(
                            containerColor = when {
                                !opt.kind.isAllow -> MaterialTheme.colorScheme.errorContainer
                                opt.kind.name == "AllowOnce" -> attention.copy(alpha = 0.18f)
                                else -> MaterialTheme.colorScheme.surfaceVariant
                            },
                            contentColor = if (opt.kind.isAllow) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onErrorContainer,
                        ),
                    ) {
                        if (sent == opt.id) { CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 2.dp, color = MaterialTheme.colorScheme.onSurfaceVariant); Spacer(Modifier.width(6.dp)) }
                        Text(opt.label)
                    }
                }
            }
            Text("Answer here or in the terminal", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        } else item.resolution?.let {
            Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun PendingRow(p: SessionTimeline.PendingPrompt, dismiss: () -> Unit) {
    val clipboard = LocalContext.current.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    val label = when {
        p.delivered -> "Handed to agent"
        p.queued -> "Queued on Mac"
        p.unconfirmed -> "Delivery unconfirmed"
        else -> "Sending…"
    }
    Row(Modifier.fillMaxWidth()) {
        Spacer(Modifier.weight(1f).width(40.dp))
        Column(horizontalAlignment = Alignment.End) {
            Text(p.text, Modifier.background(Palette.accent.copy(alpha = 0.15f), RoundedCornerShape(16.dp)).padding(horizontal = 14.dp, vertical = 10.dp))
            Text(label, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (p.unconfirmed) {
                Row {
                    TextButton(onClick = { clipboard.setPrimaryClip(ClipData.newPlainText("Prompt", p.text)) }) {
                        Text("Copy")
                    }
                    TextButton(onClick = dismiss) { Text("Dismiss") }
                }
            }
        }
    }
}

/** Message box with dictation and send. */
@Composable
private fun Composer(connection: DaemonConnection, sessionId: String, status: SessionStatus, agent: AgentKind, canStop: Boolean) {
    val context = LocalContext.current
    var text by remember { mutableStateOf("") }
    var before by remember { mutableStateOf("") }
    val dictation = remember { Dictation(context) { heard -> text = before + heard } }
    DisposableEffect(Unit) { onDispose { dictation.destroy() } }
    val mic = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) { before = if (text.isEmpty() || text.endsWith(" ")) text else "$text "; dictation.start() }
        else connection.lastError = "Allow the microphone for Styrman to dictate."
    }
    val tint = Palette.accent
    val canSend = connection.state == DaemonConnection.State.Connected && status != SessionStatus.Done && text.isNotBlank()
    // Nothing typed while it works: the send button becomes stop (Esc on the Mac).
    val showsStop = canStop && text.isBlank() && (status == SessionStatus.Working || status == SessionStatus.NeedsApproval)

    Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp)) {
        if (status == SessionStatus.Done) Text("This session has ended.", style = MaterialTheme.typography.labelSmall)
        Row(verticalAlignment = Alignment.Bottom) {
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                placeholder = { Text("Message ${agent.displayName}") },
                modifier = Modifier.weight(1f),
                maxLines = 6,
                enabled = status != SessionStatus.Done,
                shape = RoundedCornerShape(20.dp),
            )
            if (dictation.available) IconButton(
                onClick = {
                    if (dictation.listening) dictation.stop()
                    else if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
                        before = if (text.isEmpty() || text.endsWith(" ")) text else "$text "
                        dictation.start()
                    } else mic.launch(Manifest.permission.RECORD_AUDIO)
                },
                enabled = status != SessionStatus.Done,
            ) {
                Icon(if (dictation.listening) Icons.Filled.Stop else Icons.Filled.Mic,
                    if (dictation.listening) "Stop dictation" else "Dictate",
                    tint = if (dictation.listening) tint else MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (showsStop) IconButton(
                onClick = { connection.interrupt(sessionId) },
                enabled = connection.state == DaemonConnection.State.Connected,
            ) {
                Icon(Icons.Filled.StopCircle, "Stop ${agent.displayName}", tint = MaterialTheme.colorScheme.onSurface)
            } else IconButton(onClick = {
                dictation.stop()
                if (connection.sendPrompt(sessionId, text)) text = ""
            }, enabled = canSend) {
                Icon(Icons.AutoMirrored.Filled.Send, "Send", tint = if (canSend) tint else MaterialTheme.colorScheme.outline)
            }
        }
    }
}

/** A message another agent sent through Styrman (`styr ask`), as the receiving agent read it. */
data class AgentMessage(val sender: String, val agent: AgentKind, val body: String)

fun agentMessage(text: String): AgentMessage? {
    val prefix = "Message from "
    if (!text.startsWith(prefix)) return null
    val head = text.indexOf(", sent through Styrman:").takeIf { it > 0 } ?: return null
    val sender = text.substring(prefix.length, head)
    var rest = text.substring(head + ", sent through Styrman:".length).trim()
    val tail = rest.lastIndexOf("\n\n(Reply as you normally would")
    if (tail >= 0) rest = rest.substring(0, tail).trim()
    val lower = sender.lowercase()
    val agent = when {
        lower.startsWith("claude") -> AgentKind.Claude
        lower.startsWith("codex") -> AgentKind.Codex
        lower.startsWith("agy") -> AgentKind.Antigravity
        else -> AgentKind.Unknown
    }
    return AgentMessage(sender, agent, rest)
}

@Composable
private fun AgentMessageRow(m: AgentMessage) {
    val c = agentColor(m.agent)
    Column(
        Modifier.fillMaxWidth()
            .background(c.copy(alpha = 0.10f), RoundedCornerShape(14.dp))
            .border(1.dp, c.copy(alpha = 0.35f), RoundedCornerShape(14.dp))
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            AgentAvatar(m.agent, 18.dp)
            Spacer(Modifier.width(6.dp))
            Text("From ${m.sender} →", color = c, fontWeight = FontWeight.SemiBold, style = MaterialTheme.typography.labelMedium)
        }
        SelectionContainer { Text(m.body) }
    }
}


/** The session's overflow menu: mode, changes, terminal, ending and sharing. */
@Composable
private fun SessionMenu(
    s: SessionSummary,
    expanded: Boolean,
    onDismiss: () -> Unit,
    onMode: () -> Unit,
    onChanges: () -> Unit,
    onTerminal: () -> Unit,
    onEnd: () -> Unit,
    onStopSharing: () -> Unit,
) {
    fun pick(action: () -> Unit) = { onDismiss(); action() }
    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss) {
        val mode = s.permissionMode
        if (mode != null && s.status != SessionStatus.Done) {
            DropdownMenuItem(
                text = { Text("Mode: ${mode.label}") },
                leadingIcon = { Icon(Icons.Filled.Tune, null) },
                onClick = pick(onMode),
            )
        }
        if (s.cwd != null) DropdownMenuItem(
            text = { Text("Changes") }, leadingIcon = { Icon(Icons.Filled.Difference, null) }, onClick = pick(onChanges),
        )
        if (s.hasLiveControls) DropdownMenuItem(
            text = { Text("Terminal") }, leadingIcon = { Icon(Icons.Filled.Terminal, null) }, onClick = pick(onTerminal),
        )
        if (s.outdated && s.inTerminalWrapper) {
            // Explains why Stop and Terminal are missing here.
            DropdownMenuItem(
                text = {
                    Column {
                        Text("Started before an update")
                        Text("End it and start it again for Stop and Terminal.", style = MaterialTheme.typography.bodySmall)
                    }
                },
                leadingIcon = { Icon(Icons.Filled.RestartAlt, null) },
                onClick = {},
                enabled = false,
            )
        }
        HorizontalDivider()
        val danger = MaterialTheme.colorScheme.error
        if (s.canEnd) DropdownMenuItem(
            text = { Text("End session", color = danger) },
            leadingIcon = { Icon(Icons.Filled.StopCircle, null, tint = danger) },
            onClick = pick(onEnd),
        )
        DropdownMenuItem(
            text = { Text(if (s.status == SessionStatus.Done) "Remove" else "Stop sharing", color = danger) },
            leadingIcon = { Icon(Icons.Filled.VisibilityOff, null, tint = danger) },
            onClick = pick(onStopSharing),
        )
    }
}

/** Claude's permission modes, as Shift+Tab cycles them in its terminal. */
@Composable
private fun ModeDialog(current: PermissionMode, onDismiss: () -> Unit, choose: (PermissionMode) -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Permission mode") },
        text = {
            Column {
                PermissionMode.choices(current).forEach { m ->
                    Row(
                        Modifier.fillMaxWidth().clickable { if (m == current) onDismiss() else choose(m) }.padding(vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(selected = m == current, onClick = null)
                        Spacer(Modifier.width(12.dp))
                        Column {
                            Text(m.label)
                            Text(m.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** The reply being written right now, as the terminal shows it: grows, with a blinking
 *  cursor. The real message replaces it when it's done. */
@Composable
private fun LivePreviewRow(text: String, agent: AgentKind) {
    val tint = agentColor(agent)
    val blink by rememberInfiniteTransition(label = "cursor").animateFloat(
        initialValue = 1f, targetValue = 0.15f,
        animationSpec = infiniteRepeatable(tween(550), RepeatMode.Reverse),
        label = "cursor",
    )
    val body = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.85f)
    Row(Modifier.fillMaxWidth().height(androidx.compose.foundation.layout.IntrinsicSize.Min)) {
        Box(Modifier.width(3.dp).fillMaxHeight().background(tint.copy(alpha = 0.25f), RoundedCornerShape(2.dp)))
        Text(
            androidx.compose.ui.text.buildAnnotatedString {
                append(text)
                append(" ")
                pushStyle(androidx.compose.ui.text.SpanStyle(color = tint.copy(alpha = blink)))
                append("▍")
                pop()
            },
            color = body,
            modifier = Modifier.padding(start = 12.dp).semantics { contentDescription = "${agent.displayName} is writing: $text" },
        )
    }
}
