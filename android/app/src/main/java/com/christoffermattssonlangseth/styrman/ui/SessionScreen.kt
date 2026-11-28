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
import androidx.compose.foundation.layout.offset
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
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.Warning
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.AgentKind
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.Dictation
import com.christoffermattssonlangseth.styrman.FileDiff
import com.christoffermattssonlangseth.styrman.WorkStatus
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
    val status = timeline.status?.takeIf { summary?.status == SessionStatus.Working }
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
    var renaming by remember { mutableStateOf(false) }
    var pickMode by remember { mutableStateOf(false) }
    var showChanges by remember { mutableStateOf(false) }
    var showTerminal by remember { mutableStateOf(false) }

    Scaffold(
        topBar = {
          Column {
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
                            Text(summary?.let { it.name ?: it.repoName } ?: "Session", maxLines = 1, overflow = TextOverflow.Ellipsis)
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
                                onRename = { renaming = true },
                                onStopSharing = { connection.stopSharing(sessionId); onBack() },
                            )
                        }
                    }
                },
            )
            summary?.let { HelperLinks(it, connection) }
          }
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
            // An answered ask shows with its reply, where the reply came.
            val exchanges = PeerNote.exchanges(timeline.items)
            val answered = exchanges.map { it.askId }.toSet()
            val openAsks = PeerNote.openAsks(timeline.items)
            val rows = DisplayRow.rows(timeline.items.filter { it.id !in answered })
            items(rows, key = { it.id }) { row ->
                when (row) {
                    is DisplayRow.Tools -> StepsRow(row.tools, summary?.cwd)
                    is DisplayRow.Item -> TimelineRow(
                        row.item, summary?.cwd, agent, speaker, timeline,
                        Peers(sessionId, connection, openAsks, exchanges),
                    ) { request, option -> connection.resolvePermission(sessionId, request, option) }
                }
            }
            items(timeline.pending, key = { it.id }) { p ->
                PendingRow(p) { timeline.dismissUnconfirmed(p.id) }
            }
            if (preview != null) item(key = "preview") { LivePreviewRow(preview, agent) }
            // Under it, as in the terminal: what the agent is doing, and for how long.
            if (summary?.status == SessionStatus.Working && (status != null || preview == null)) item(key = "status") {
                StatusRow(status, timeline.statusAtMs, timeline.thinkingSinceMs, agent)
            }
            if (timeline.runningTasks.isNotEmpty()) item(key = "tasks") { RunningTasksLink(timeline, summary?.cwd) }
            if (timeline.isLive && !timeline.hasContent && timeline.pending.isEmpty()) item {
                Notice("Nothing yet. Prompts, tool calls and replies show up here as they happen.")
            }
        }
    }
    if (renaming && summary != null) RenameDialog(summary, onDismiss = { renaming = false }) {
        connection.rename(sessionId, it); renaming = false
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

/** What a row needs to show agents talking to each other. */
private class Peers(val sessionId: String, val connection: DaemonConnection, val openAsks: Set<String>, val exchanges: List<PeerExchange>)

@Composable
private fun TimelineRow(
    item: TimelineItem,
    cwd: String?,
    agent: AgentKind,
    speaker: Speaker,
    timeline: SessionTimeline,
    peers: Peers,
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
        is TimelineItem.Tool -> StepsRow(listOf(item), cwd)
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
        is TimelineItem.Notice -> {
            val exchange = peers.exchanges.firstOrNull { it.id == item.id }
            val note = if (exchange == null) PeerNote.parse(item.text) else null
            when {
                exchange != null -> ExchangeCard(exchange, agent, peers.sessionId, peers.exchanges, peers.connection)
                note != null -> PeerCard(note, agent, peers.sessionId, item.id in peers.openAsks, peers.connection)
                // Another agent's reply quoted here is Markdown: bold as bold, not `**`.
                else -> Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    val tint = if (item.warning) Color(0xFFF29A2E) else MaterialTheme.colorScheme.onSurfaceVariant
                    Icon(if (item.warning) Icons.Filled.Warning else Icons.Filled.Notifications, null, tint = tint,
                        modifier = Modifier.size(14.dp).padding(top = 2.dp))
                    SelectionContainer {
                        Text(markdown(item.text, MaterialTheme.colorScheme.surfaceContainerHighest),
                            style = MaterialTheme.typography.bodySmall, color = tint)
                    }
                }
            }
        }
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
    onRename: () -> Unit,
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
        DropdownMenuItem(
            text = { Text("Rename…") }, leadingIcon = { Icon(Icons.Filled.Edit, null) }, onClick = pick(onRename),
        )
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

/** "Rename…": the session's name, shown here and on its cmux tab. Blank: its own name again. */
@Composable
private fun RenameDialog(s: SessionSummary, onDismiss: () -> Unit, save: (String) -> Unit) {
    var text by remember { mutableStateOf(s.name ?: "") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Rename session") },
        text = {
            androidx.compose.material3.OutlinedTextField(
                value = text, onValueChange = { text = it }, singleLine = true,
                placeholder = { Text(s.repoName) },
                supportingText = { Text("Shown here and on its tab on the Mac.") },
            )
        },
        confirmButton = { TextButton(onClick = { save(text) }) { Text("Save") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
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

/** While the agent works, as the Claude app shows it: four dots taking turns and the
 *  word Claude picked ("Brewing…") in its colour, a lighter band passing over it; while
 *  it thinks, or just after, how long it's been ("26s · still thinking…"). */
@Composable
private fun StatusRow(status: WorkStatus?, sinceMs: Long, thinkingSinceMs: Long?, agent: AgentKind) {
    val tint = agentColor(agent)
    var now by androidx.compose.runtime.remember { androidx.compose.runtime.mutableLongStateOf(System.currentTimeMillis()) }
    androidx.compose.runtime.LaunchedEffect(Unit) {
        while (true) { kotlinx.coroutines.delay(70); now = System.currentTimeMillis() }
    }
    val time = timeLine(status, (now - sinceMs) / 1000, thinkingSinceMs?.let { (now - it) / 1000 })
    val label = status?.label ?: "Working…"
    val lit = ((now / 180) % 4).toInt()
    Row(
        Modifier.fillMaxWidth().semantics { contentDescription = "${agent.displayName} is working" },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.width(22.dp).height(14.dp)) {
            listOf(0 to -4, 4 to 0, 0 to 4, -4 to 0).forEachIndexed { i, (x, y) ->
                Box(
                    Modifier.align(Alignment.Center)
                        .offset(x = x.dp, y = y.dp)
                        .size(3.5.dp)
                        .background(tint.copy(alpha = if (i == lit) 1f else 0.4f), androidx.compose.foundation.shape.CircleShape),
                )
            }
        }
        if (time != null) {
            Text(time, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
        } else {
            val head = ((now / 70) % (label.length + 14)).toInt() - 3
            Text(
                androidx.compose.ui.text.buildAnnotatedString {
                    label.forEachIndexed { i, ch ->
                        val d = kotlin.math.abs(i - head)
                        pushStyle(androidx.compose.ui.text.SpanStyle(color = tint.copy(alpha = if (d <= 2) 0.45f + 0.2f * d else 1f)))
                        append(ch)
                        pop()
                    }
                },
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

/** While it thinks or right after: "26s · still thinking…", "22s · thought for 1s".
 *  Null otherwise (the word shows). */
internal fun timeLine(s: WorkStatus?, extra: Long, thinkingFor: Long?): String? {
    val secs = s?.elapsedSecs ?: return null
    val time = WorkStatus.duration(secs + maxOf(0, extra))
    if (s.thinking) return "$time · ${if ((thinkingFor ?: 0) >= 8) "still thinking…" else "thinking…"}"
    val note = s.note
    if (note != null && note.startsWith("thought for")) return "$time · $note"
    return null
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
