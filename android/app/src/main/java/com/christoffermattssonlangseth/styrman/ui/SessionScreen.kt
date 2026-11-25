package com.christoffermattssonlangseth.styrman.ui

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
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
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.VolumeOff
import androidx.compose.material.icons.filled.VolumeUp
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.AgentKind
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.Dictation
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
    LaunchedEffect(timeline.items.size + timeline.pending.size) {
        val n = timeline.items.size + timeline.pending.size
        if (n > 0) list.animateScrollToItem(n)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                // The bar takes the agent's colour, so it's always clear who you're talking to.
                colors = androidx.compose.material3.TopAppBarDefaults.topAppBarColors(
                    containerColor = agentColor(agent).copy(alpha = 0.22f)
                        .compositeOver(MaterialTheme.colorScheme.surface),
                ),
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (summary != null) { AgentAvatar(summary.agent, 26.dp); Spacer(Modifier.width(8.dp)) }
                        Text(summary?.repoName ?: "Session", maxLines = 1, overflow = TextOverflow.Ellipsis)
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
                    summary?.let { StatusChip(it.status); Spacer(Modifier.width(8.dp)) }
                },
            )
        },
        bottomBar = {
            Surface(tonalElevation = 3.dp, modifier = Modifier.imePadding().navigationBarsPadding()) {
                when {
                    summary == null && connection.state == DaemonConnection.State.Connected ->
                        Notice("This session is no longer shared.")
                    summary == null -> {}
                    summary.acceptsPrompts -> Composer(connection, sessionId, summary.status, agent)
                    else -> Notice(
                        "Read-only: this ${agent.displayName} session wasn't started through Styrman. " +
                            "Run `styr setup` on the Mac, then start it again in a new terminal tab.",
                    )
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
                TimelineRow(item, summary?.cwd, agent, speaker) { request, option ->
                    connection.resolvePermission(sessionId, request, option)
                }
            }
            items(timeline.pending) { p -> PendingRow(p, summary?.status == SessionStatus.Working, agent) }
            if (!timeline.isLive) item { Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) { CircularProgressIndicator() } }
            else if (!timeline.hasContent && timeline.pending.isEmpty()) item {
                Notice("Nothing yet. Prompts, tool calls and replies show up here as they happen.")
            }
        }
    }
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
    answer: (String, String) -> Boolean,
) {
    when (item) {
        is TimelineItem.Divider -> Text(
            item.text, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.fillMaxWidth(), textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
        is TimelineItem.User -> Row(Modifier.fillMaxWidth()) {
            Spacer(Modifier.weight(1f).width(40.dp))
            Column(horizontalAlignment = Alignment.End) {
                SelectionContainer {
                    Text(item.text, Modifier.background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(16.dp)).padding(horizontal = 14.dp, vertical = 10.dp))
                }
                if (item.fromPhone) Text("From this phone", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        is TimelineItem.Assistant -> Reply(item, agent, speaker)
        is TimelineItem.Tool -> ToolRow(item, displayTitle(item.tool, cwd))
        is TimelineItem.Permission -> PermissionCard(item, displayTitle(item.tool, cwd), answer)
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
private fun ToolRow(item: TimelineItem.Tool, title: String) {
    var open by remember { mutableStateOf(false) }
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
private fun PermissionCard(item: TimelineItem.Permission, title: String, answer: (String, String) -> Boolean) {
    var sent by remember(item.request) { mutableStateOf<String?>(null) }
    val accent = Palette.accent
    Column(
        Modifier.fillMaxWidth()
            .background(if (item.isOpen) accent.copy(alpha = 0.08f) else Color.Transparent, RoundedCornerShape(14.dp))
            .border(1.dp, if (item.isOpen) accent.copy(alpha = 0.6f) else MaterialTheme.colorScheme.outlineVariant, RoundedCornerShape(14.dp))
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Icon(Icons.Filled.PanTool, null, tint = if (item.isOpen) accent else MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(16.dp))
            Text(if (item.isOpen) "Permission needed" else "Permission", fontWeight = FontWeight.SemiBold,
                color = if (item.isOpen) accent else MaterialTheme.colorScheme.onSurfaceVariant)
        }
        SelectionContainer { Text(title, fontFamily = if (item.tool.kind == ToolKind.Execute) Mono else null) }
        item.reason?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        if (item.isOpen) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                item.options.forEach { opt ->
                    Button(
                        onClick = { if (answer(item.request, opt.id)) sent = opt.id },
                        enabled = sent == null,
                        colors = ButtonDefaults.buttonColors(
                            containerColor = if (opt.kind.isAllow) (if (opt.kind.name == "AllowOnce") Palette.ink else Color.Gray) else accent,
                            contentColor = Color.White,
                        ),
                    ) {
                        if (sent == opt.id) { CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 2.dp, color = Color.White); Spacer(Modifier.width(6.dp)) }
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
private fun PendingRow(p: SessionTimeline.PendingPrompt, working: Boolean, agent: AgentKind) {
    val label = when {
        p.delivered -> if (working) "Delivered; ${agent.displayName} picks it up after the current step" else "Delivered"
        p.queued || working -> "Queued, sent when ${agent.displayName} finishes"
        else -> "Sending…"
    }
    Row(Modifier.fillMaxWidth()) {
        Spacer(Modifier.weight(1f).width(40.dp))
        Column(horizontalAlignment = Alignment.End) {
            Text(p.text, Modifier.background(Palette.accent.copy(alpha = 0.15f), RoundedCornerShape(16.dp)).padding(horizontal = 14.dp, vertical = 10.dp))
            Text(label, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/** Message box with dictation and send, tinted in the agent's colour. */
@Composable
private fun Composer(connection: DaemonConnection, sessionId: String, status: SessionStatus, agent: AgentKind) {
    val context = LocalContext.current
    var text by remember { mutableStateOf("") }
    var before by remember { mutableStateOf("") }
    val dictation = remember { Dictation(context) { heard -> text = before + heard } }
    DisposableEffect(Unit) { onDispose { dictation.destroy() } }
    val mic = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) { before = if (text.isEmpty() || text.endsWith(" ")) text else "$text "; dictation.start() }
        else connection.lastError = "Allow the microphone for Styrman to dictate."
    }
    val tint = agentColor(agent)
    val canSend = connection.state == DaemonConnection.State.Connected && status != SessionStatus.Done && text.isNotBlank()

    Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp)) {
        if (status == SessionStatus.Working) Text("${agent.displayName} is working. Your message is sent when it finishes.", style = MaterialTheme.typography.labelSmall)
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
            IconButton(onClick = {
                dictation.stop()
                if (connection.sendPrompt(sessionId, text)) text = ""
            }, enabled = canSend) {
                Icon(Icons.AutoMirrored.Filled.Send, "Send", tint = if (canSend) tint else MaterialTheme.colorScheme.outline)
            }
        }
    }
}
