package com.christoffermattssonlangseth.styrman.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.AgentTask
import com.christoffermattssonlangseth.styrman.FileDiff
import com.christoffermattssonlangseth.styrman.SessionTimeline
import com.christoffermattssonlangseth.styrman.TimelineItem
import com.christoffermattssonlangseth.styrman.ToolKind
import com.christoffermattssonlangseth.styrman.ToolStatus
import com.christoffermattssonlangseth.styrman.WorkStatus
import com.christoffermattssonlangseth.styrman.str
import kotlinx.coroutines.delay

/** A row of the conversation: an item, or a run of tool calls shown as one line. */
sealed interface DisplayRow {
    val id: String

    data class Item(val item: TimelineItem) : DisplayRow { override val id get() = item.id }
    data class Tools(override val id: String, val tools: List<TimelineItem.Tool>) : DisplayRow

    companion object {
        fun rows(items: List<TimelineItem>): List<DisplayRow> {
            val out = mutableListOf<DisplayRow>()
            val run = mutableListOf<TimelineItem.Tool>()
            fun flush() {
                if (run.isNotEmpty()) out += Tools(run.first().id, run.toList())
                run.clear()
            }
            for (item in items) {
                if (item is TimelineItem.Tool) run += item else { flush(); out += Item(item) }
            }
            flush()
            return out
        }
    }
}

object Steps {
    /** What one step says: Claude's description of a command ("Run the parser tests"),
     *  else the command or the tool's title. */
    fun title(t: TimelineItem.Tool, cwd: String?): String =
        (if (t.tool.kind == ToolKind.Execute) (t.tool.input as? kotlinx.serialization.json.JsonObject)?.str("description") else null)
            ?: displayTitle(t.tool, cwd)

    /** A step in progress: "Running", "Reading", … */
    fun doing(kind: ToolKind) = when (kind) {
        ToolKind.Execute -> "Running"
        ToolKind.Read -> "Reading"
        ToolKind.Edit -> "Editing"
        ToolKind.Delete -> "Deleting"
        ToolKind.Move -> "Moving"
        ToolKind.Search -> "Searching"
        ToolKind.Fetch -> "Fetching"
        ToolKind.Think -> "Planning"
        ToolKind.Mcp, ToolKind.Other -> "Working"
    }

    /** "Ran 3 commands, read a file (1 failed)", kinds in the order they came. */
    fun summary(tools: List<TimelineItem.Tool>): String {
        val counts = LinkedHashMap<String, Int>()
        for (t in tools) {
            val key = when (t.tool.kind) {
                ToolKind.Execute -> "ran|command|commands"
                ToolKind.Read -> "read|file|files"
                ToolKind.Edit -> "edited|file|files"
                ToolKind.Delete -> "deleted|file|files"
                ToolKind.Move -> "moved|file|files"
                ToolKind.Search -> "searched|time|times"
                ToolKind.Fetch -> "fetched|page|pages"
                ToolKind.Think -> "updated|plan|plans"
                ToolKind.Mcp, ToolKind.Other -> "used|tool|tools"
            }
            counts[key] = (counts[key] ?: 0) + 1
        }
        var text = counts.entries.joinToString(", ") { (key, n) ->
            val w = key.split("|")
            when {
                w[1] == "time" -> if (n == 1) "searched once" else "searched $n times"
                w[1] == "plan" -> "updated the plan"
                n == 1 -> "${w[0]} a ${w[1]}"
                else -> "${w[0]} $n ${w[2]}"
            }
        }
        val failed = tools.count { it.status == ToolStatus.Failed }
        if (failed > 0) text += " ($failed failed)"
        return text.replaceFirstChar { it.uppercase() }
    }

    /** Long output shows its first part (a whole log would make scrolling crawl). */
    const val MAX_OUTPUT = 20_000

    fun outputText(t: TimelineItem.Tool): String {
        val o = t.output ?: return if (t.status == ToolStatus.Completed || t.status == ToolStatus.Failed) "(no output)" else "Still running…"
        if (o.isEmpty()) return "(no output)"
        return if (o.length > MAX_OUTPUT) o.take(MAX_OUTPUT) + "\n…" else o
    }

    /** "28s", "4m 2s", "1h 11m", like the Claude app's. */
    fun taskDuration(ms: Long): String {
        val s = maxOf(0, ms / 1000)
        if (s >= 3600) return "${s / 3600}h ${s % 3600 / 60}m"
        return WorkStatus.duration(s)
    }
}

/** A shell command coloured the way the Claude app does: commands in orange, quoted
 *  strings in green, the rest plain. */
object ShellHighlight {
    val command = Color(0xFFF28C59)   // (0.95, 0.55, 0.35)
    val string = Color(0xFF8CC773)    // (0.55, 0.78, 0.45)

    enum class Kind { Plain, Command, Str }

    /** The command in pieces, each with how it's coloured. */
    fun segments(cmd: String): List<Pair<String, Kind>> {
        val out = mutableListOf<Pair<String, Kind>>()
        val word = StringBuilder()
        var atStart = true
        var quote: Char? = null
        val str = StringBuilder()
        fun flushWord() {
            if (word.isEmpty()) return
            val w = word.toString()
            // `VAR=value` before a command isn't the command.
            if (atStart && !w.contains('=')) { out += w to Kind.Command; atStart = false } else out += w to Kind.Plain
            word.clear()
        }
        for (c in cmd) {
            val q = quote
            if (q != null) {
                str.append(c)
                if (c == q) { out += str.toString() to Kind.Str; str.clear(); quote = null }
                continue
            }
            when (c) {
                '\'', '"' -> { flushWord(); atStart = false; quote = c; str.append(c) }
                ' ', '\t' -> { flushWord(); out += c.toString() to Kind.Plain }
                '\n', ';', '|', '&', '(' -> { flushWord(); out += c.toString() to Kind.Plain; atStart = true }
                else -> word.append(c)
            }
        }
        flushWord()
        if (str.isNotEmpty()) out += str.toString() to Kind.Str
        return out
    }

    fun annotated(cmd: String): AnnotatedString = buildAnnotatedString {
        for ((text, kind) in segments(cmd)) when (kind) {
            Kind.Plain -> append(text)
            Kind.Command -> withStyle(SpanStyle(color = command)) { append(text) }
            Kind.Str -> withStyle(SpanStyle(color = string)) { append(text) }
        }
    }
}

/** A step's mark: `>_` for a command, the tool's icon otherwise. */
@Composable
fun StepIcon(kind: ToolKind, tint: Color = MaterialTheme.colorScheme.onSurfaceVariant) {
    if (kind == ToolKind.Execute) Text(">_", fontFamily = Mono, color = tint, style = MaterialTheme.typography.labelMedium, maxLines = 1, softWrap = false)
    else Icon(toolIcon(kind), null, tint = tint, modifier = Modifier.size(16.dp))
}

/** A word with a lighter band passing over it, as the Claude app shows work in progress. */
@Composable
fun Shimmer(text: String, color: Color, modifier: Modifier = Modifier, maxLines: Int = 1) {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) { while (true) { delay(70); now = System.currentTimeMillis() } }
    val head = ((now / 70) % (text.length + 14)).toInt() - 3
    Text(
        buildAnnotatedString {
            text.forEachIndexed { i, ch ->
                val d = kotlin.math.abs(i - head)
                withStyle(SpanStyle(color = color.copy(alpha = if (d <= 2) 0.45f + 0.2f * d else 1f))) { append(ch) }
            }
        },
        modifier = modifier, maxLines = maxLines, overflow = TextOverflow.Ellipsis,
    )
}

/** A run of steps, as the Claude app shows it: one quiet line. "Ran 3 commands, read a
 *  file", a single step's description, or while one runs, "Running". Tap for the steps,
 *  their commands and output. */
@Composable
fun StepsRow(tools: List<TimelineItem.Tool>, cwd: String?) {
    var open by remember { mutableStateOf(false) }
    val last = tools.last()
    val running = last.status == ToolStatus.Pending || last.status == ToolStatus.Running
    val failed = tools.count { it.status == ToolStatus.Failed }
    val grey = MaterialTheme.colorScheme.onSurfaceVariant
    Row(
        Modifier.fillMaxWidth().clickable { open = true }.padding(vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Box(Modifier.width(20.dp), contentAlignment = Alignment.Center) {
            if (tools.size > 1 && !running) Icon(Icons.AutoMirrored.Filled.List, null, tint = grey, modifier = Modifier.size(16.dp))
            else StepIcon(last.tool.kind)
        }
        if (running) Shimmer(Steps.doing(last.tool.kind), grey)
        else {
            Text(
                if (tools.size == 1) Steps.title(tools[0], cwd) else Steps.summary(tools),
                Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis,
                color = grey, style = MaterialTheme.typography.bodyMedium,
            )
            if (tools.size == 1 && tools[0].tool.kind == ToolKind.Edit) FileDiff.from(tools[0].tool)?.let { DiffCounts(it) }
            if (failed > 0) Icon(Icons.Filled.Warning, "Failed", tint = Color(0xFFF29A2E), modifier = Modifier.size(14.dp))
        }
        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null, tint = grey, modifier = Modifier.size(16.dp))
    }
    if (open) {
        if (tools.size == 1) ToolDetailSheet(tools[0], cwd) { open = false }
        else StepsSheet(tools, cwd) { open = false }
    }
}

/** A sheet with a title row and a scrolling body. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DetailSheet(title: String, onDismiss: () -> Unit, full: Boolean = false, content: @Composable () -> Unit) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = androidx.compose.material3.rememberModalBottomSheetState(skipPartiallyExpanded = full),
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(bottom = 24.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold,
                maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(bottom = 12.dp))
            Column(Modifier.verticalScroll(rememberScrollState())) { content() }
        }
    }
}

/** One step: the tool's name as title, what it was for, its command, changes, output. */
@Composable
fun ToolDetailSheet(item: TimelineItem.Tool, cwd: String?, onDismiss: () -> Unit) {
    DetailSheet(item.tool.name, onDismiss) { StepDetail(item, cwd) }
}

/** A step's details: what it was for (unless [heading] is off), its command or input,
 *  an edit's changes, and its output. */
@Composable
fun StepDetail(item: TimelineItem.Tool, cwd: String?, heading: Boolean = true) {
    val shell = item.tool.kind == ToolKind.Execute
    val title = displayTitle(item.tool, cwd)
    val out = Steps.outputText(item)
    Column(verticalArrangement = Arrangement.spacedBy(18.dp), modifier = Modifier.fillMaxWidth()) {
        val description = (item.tool.input as? kotlinx.serialization.json.JsonObject)?.str("description")
        if (heading && shell && description != null) Text(description, style = MaterialTheme.typography.titleLarge)
        DetailCard(if (shell) "Command" else "Input", title) {
            Text(if (shell) ShellHighlight.annotated(title) else AnnotatedString(title), fontFamily = Mono, style = MaterialTheme.typography.bodySmall)
        }
        if (item.tool.kind == ToolKind.Edit) FileDiff.from(item.tool)?.let { diffs ->
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Changes", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
                DiffPreview(diffs, cwd)
            }
        }
        DetailCard("Output", out) { Text(out, fontFamily = Mono, style = MaterialTheme.typography.bodySmall) }
    }
}

/** "Command" / "Output": a label with a copy button, the text in a rounded card. */
@Composable
fun DetailCard(label: String, copy: String, content: @Composable () -> Unit) {
    val context = LocalContext.current
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) { if (copied) { delay(1500); copied = false } }
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(label, Modifier.weight(1f), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
            IconButton(onClick = {
                (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText(label, copy))
                copied = true
            }, modifier = Modifier.size(32.dp)) {
                Icon(if (copied) Icons.Filled.Check else Icons.Filled.ContentCopy, "Copy ${label.lowercase()}",
                    tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(16.dp))
            }
        }
        Box(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainerHigh, RoundedCornerShape(16.dp)).padding(14.dp)) {
            SelectionContainer { content() }
        }
    }
}

/** The steps of a run, as the Claude app lists them: one under another, joined by a
 *  thin line, each opening in place to its command and output. */
@Composable
fun StepsSheet(tools: List<TimelineItem.Tool>, cwd: String?, onDismiss: () -> Unit) {
    DetailSheet(Steps.summary(tools), onDismiss) {
        tools.forEachIndexed { i, t -> StepEntry(t, cwd, last = i == tools.lastIndex) }
    }
}

@Composable
private fun StepEntry(item: TimelineItem.Tool, cwd: String?, last: Boolean) {
    var open by remember { mutableStateOf(false) }
    val grey = MaterialTheme.colorScheme.onSurfaceVariant
    Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Column(Modifier.width(22.dp).fillMaxHeight(), horizontalAlignment = Alignment.CenterHorizontally) {
            Box(Modifier.height(22.dp), contentAlignment = Alignment.Center) { StepIcon(item.tool.kind) }
            if (!last) Box(Modifier.padding(top = 6.dp).width(1.dp).weight(1f).background(grey.copy(alpha = 0.3f)))
        }
        Column(Modifier.weight(1f).padding(bottom = if (last) 0.dp else 22.dp).animateContentSize(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(Modifier.fillMaxWidth().clickable { open = !open }, verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(Steps.title(item, cwd), Modifier.weight(1f), maxLines = if (open) Int.MAX_VALUE else 1, overflow = TextOverflow.Ellipsis)
                when (item.status) {
                    ToolStatus.Failed -> Icon(Icons.Filled.Warning, "Failed", tint = Color(0xFFF29A2E), modifier = Modifier.size(14.dp))
                    ToolStatus.Running, ToolStatus.Pending -> CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp)
                    else -> {}
                }
                Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, if (open) "Close" else "Open", tint = grey,
                    modifier = Modifier.size(16.dp).rotate(if (open) 90f else 0f))
            }
            if (open) StepDetail(item, cwd, heading = false)
        }
    }
}

/** "3 running tasks", as the Claude app has it under the conversation. Tap for the list. */
@Composable
fun RunningTasksLink(timeline: SessionTimeline, cwd: String?) {
    var open by remember { mutableStateOf(false) }
    val n = timeline.runningTasks.size
    Text(
        if (n == 1) "1 running task" else "$n running tasks",
        color = Color(0xFF3B82F6), textDecoration = TextDecoration.Underline,
        modifier = Modifier.clickable { open = true }.padding(top = 4.dp),
    )
    if (open) BackgroundTasksSheet(timeline, cwd) { open = false }
}

/** The background tasks: running ones first, then those that finished. */
@Composable
fun BackgroundTasksSheet(timeline: SessionTimeline, cwd: String?, onDismiss: () -> Unit) {
    var showRunning by remember { mutableStateOf(true) }
    var showDone by remember { mutableStateOf(false) }
    val running = timeline.runningTasks
    val done = timeline.tasks.filter { it.ended != null }.asReversed().take(30)
    DetailSheet("Background tasks", onDismiss) {
        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            TaskSection("Running", showRunning) { showRunning = !showRunning }
            if (showRunning) {
                if (running.isEmpty()) Text("Nothing running now.", color = MaterialTheme.colorScheme.onSurfaceVariant)
                running.forEach { TaskCard(it, cwd) }
            }
            if (done.isNotEmpty()) {
                Spacer(Modifier.height(4.dp))
                TaskSection("Finished", showDone) { showDone = !showDone }
                if (showDone) done.forEach { TaskCard(it, cwd) }
            }
        }
    }
}

@Composable
private fun TaskSection(title: String, open: Boolean, toggle: () -> Unit) {
    val grey = MaterialTheme.colorScheme.onSurfaceVariant
    Row(Modifier.clickable(onClick = toggle), verticalAlignment = Alignment.CenterVertically) {
        Text(title, color = grey, style = MaterialTheme.typography.labelLarge)
        Icon(Icons.Filled.KeyboardArrowDown, null, tint = grey, modifier = Modifier.size(16.dp).rotate(if (open) 0f else -90f))
    }
}

/** One background task: its mark, what it does (shimmering while it runs), "Shell" or
 *  "Agent" and how long it has run or ran. Tap for the command. */
@Composable
private fun TaskCard(task: AgentTask, cwd: String?) {
    var open by remember { mutableStateOf(false) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    if (task.ended == null) LaunchedEffect(Unit) { while (true) { delay(1000); now = System.currentTimeMillis() } }
    val grey = MaterialTheme.colorScheme.onSurfaceVariant
    Row(
        Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainerHigh, RoundedCornerShape(16.dp))
            .clickable { open = true }.padding(14.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(Modifier.size(22.dp), contentAlignment = Alignment.Center) {
            if (task.isAgent) Text("◇", color = grey) else StepIcon(ToolKind.Execute)
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            if (task.ended == null) Shimmer(task.title, MaterialTheme.colorScheme.onSurface, maxLines = 2)
            else Text(task.title, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(if (task.isAgent) "Agent" else "Shell", fontWeight = FontWeight.Medium, style = MaterialTheme.typography.bodySmall)
                Text(Steps.taskDuration((task.ended?.atMs ?: now) - task.startedMs), color = grey, style = MaterialTheme.typography.bodySmall)
                task.ended?.takeIf { it.status != "completed" }?.let {
                    Text(it.status, color = Color(0xFFF29A2E), style = MaterialTheme.typography.bodySmall)
                }
            }
            task.ended?.summary?.let { Text(it, color = grey, style = MaterialTheme.typography.bodySmall, maxLines = 2, overflow = TextOverflow.Ellipsis) }
        }
    }
    if (open) ToolDetailSheet(
        TimelineItem.Tool(task.id, task.id, task.tool, if (task.ended == null) ToolStatus.Running else ToolStatus.Completed, null), cwd,
    ) { open = false }
}
