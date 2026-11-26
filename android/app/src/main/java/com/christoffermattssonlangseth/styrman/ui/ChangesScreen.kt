package com.christoffermattssonlangseth.styrman.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Comment
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import com.christoffermattssonlangseth.styrman.AgentKind
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.FileDiff
import com.christoffermattssonlangseth.styrman.ReviewComment
import com.christoffermattssonlangseth.styrman.ReviewMessage

/** A screen over the session (a dialog, so the session stays subscribed underneath). */
@Composable
fun FullScreenDialog(onDismiss: () -> Unit, content: @Composable () -> Unit) {
    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.fillMaxSize()) { content() }
    }
}

/**
 * The session folder's uncommitted changes, file by file. Tapping a line comments on it;
 * the comments go to the agent together as one message (like reviewing a pull request).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChangesDialog(connection: DaemonConnection, sessionId: String, agent: AgentKind, onDismiss: () -> Unit) {
    var openPath by remember { mutableStateOf<String?>(null) }
    val comments = remember { mutableStateListOf<ReviewComment>() }
    val changes = connection.changes[sessionId]
    val openFile = openPath?.let { p -> changes?.files?.firstOrNull { it.path == p } }
    val tint = agentColor(agent)

    LaunchedEffect(sessionId) { connection.getChanges(sessionId) }

    FullScreenDialog(onDismiss = { if (openPath != null) openPath = null else onDismiss() }) {
        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(openFile?.path?.substringAfterLast('/') ?: "Changes", maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    navigationIcon = {
                        if (openFile != null) IconButton(onClick = { openPath = null }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                        else IconButton(onClick = onDismiss) { Icon(Icons.Filled.Close, "Close") }
                    },
                    actions = {
                        if (openFile == null) IconButton(onClick = { connection.getChanges(sessionId) }) { Icon(Icons.Filled.Refresh, "Refresh") }
                    },
                )
            },
            bottomBar = {
                if (comments.isNotEmpty()) Surface(tonalElevation = 3.dp) {
                    Button(
                        onClick = {
                            if (connection.sendPrompt(sessionId, ReviewMessage.compose(comments))) {
                                comments.clear()
                                onDismiss()
                            }
                        },
                        colors = ButtonDefaults.buttonColors(containerColor = tint),
                        modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 16.dp, vertical = 10.dp),
                    ) {
                        Text("Send ${comments.size} comment${if (comments.size == 1) "" else "s"} to ${agent.displayName}")
                    }
                }
            },
        ) { pad ->
            Box(Modifier.fillMaxSize().padding(pad)) {
                when {
                    changes == null -> Column(Modifier.align(Alignment.Center), horizontalAlignment = Alignment.CenterHorizontally) {
                        CircularProgressIndicator()
                        Spacer(Modifier.size(12.dp))
                        Text("Reading changes…", color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    changes.error != null -> Placeholder("Couldn't read the changes", changes.error) {
                        TextButton(onClick = { connection.getChanges(sessionId) }) { Text("Try again") }
                    }
                    openFile != null -> FileReview(openFile, comments)
                    changes.files.isEmpty() -> Placeholder("No changes", "Nothing uncommitted in this folder.")
                    else -> FileList(changes, comments, tint) { openPath = it.path }
                }
            }
        }
    }
}

@Composable
private fun Placeholder(title: String, detail: String, action: @Composable () -> Unit = {}) {
    Column(Modifier.fillMaxSize().padding(32.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
        Icon(Icons.Filled.CheckCircle, null, tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(32.dp))
        Spacer(Modifier.size(8.dp))
        Text(title, style = MaterialTheme.typography.titleMedium)
        Text(detail, textAlign = TextAlign.Center, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        action()
    }
}

@Composable
private fun FileList(
    changes: DaemonConnection.Changes,
    comments: List<ReviewComment>,
    tint: androidx.compose.ui.graphics.Color,
    open: (FileDiff) -> Unit,
) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    LazyColumn(contentPadding = PaddingValues(vertical = 8.dp)) {
        item {
            val n = changes.files.size
            Text(
                "$n changed file${if (n == 1) "" else "s"}" + (changes.branch?.let { " on $it" } ?: ""),
                style = MaterialTheme.typography.labelMedium, color = muted,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
        items(changes.files, key = { it.path }) { f ->
            Row(
                Modifier.fillMaxWidth().clickable { open(f) }.padding(horizontal = 16.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Icon(fileIcon(f), null, tint = muted, modifier = Modifier.size(20.dp))
                Column(Modifier.weight(1f)) {
                    Text(f.path.substringAfterLast('/'), fontWeight = FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    val dir = f.path.substringBeforeLast('/', "")
                    if (dir.isNotEmpty()) Text(dir, style = MaterialTheme.typography.bodySmall, color = muted,
                        maxLines = 1, overflow = TextOverflow.StartEllipsis)
                }
                val n = comments.count { it.path == f.path }
                if (n > 0) Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(Icons.AutoMirrored.Filled.Comment, "$n comments", tint = tint, modifier = Modifier.size(14.dp))
                    Text(" $n", style = MaterialTheme.typography.labelSmall, color = tint)
                }
                DiffCounts(listOf(f))
            }
            HorizontalDivider(Modifier.padding(start = 46.dp))
        }
        item {
            Text(
                if (changes.truncated) "Long diff: only the first part is shown."
                else "Tap a line in a file to comment on it. Comments go to the agent together.",
                style = MaterialTheme.typography.bodySmall, color = muted,
                modifier = Modifier.padding(16.dp),
            )
        }
    }
}

/** One file's diff, with line numbers; tap a line to comment on it. */
@Composable
private fun FileReview(file: FileDiff, comments: MutableList<ReviewComment>) {
    // (line, existing comment) being written.
    var editing by remember { mutableStateOf<Pair<FileDiff.Line, ReviewComment?>?>(null) }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val width = diffContentWidth(file.lines, minWidth = maxWidth, extra = 78.dp)
        // Lazy rows at one fixed width, so long files stay quick and the washes line up.
        Box(Modifier.horizontalScroll(rememberScrollState())) {
            LazyColumn(Modifier.width(width), contentPadding = PaddingValues(vertical = 8.dp)) {
                file.lines.forEachIndexed { i, l ->
                    item(key = "l$i") {
                        DiffLineRow(l, numbers = true, onClick = if (l.kind == FileDiff.Kind.Gap) null else ({ editing = l to null }))
                    }
                    val mine = comments.filter { it.path == file.path && it.line == l }
                    items(mine, key = { it.id }) { c ->
                        Row(
                            Modifier.padding(start = 56.dp, top = 4.dp, bottom = 4.dp).widthIn(max = 320.dp)
                                .background(MaterialTheme.colorScheme.primary.copy(alpha = 0.12f), RoundedCornerShape(8.dp))
                                .clickable { editing = l to c }.padding(8.dp),
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            Icon(Icons.AutoMirrored.Filled.Comment, null, modifier = Modifier.size(14.dp))
                            Text(c.text, style = MaterialTheme.typography.bodySmall)
                        }
                    }
                }
            }
        }
    }
    editing?.let { (line, existing) ->
        CommentEditor(
            place = ReviewComment(file.path, line, "").place.replace("`", ""),
            code = line.text,
            initial = existing?.text ?: "",
            onDismiss = { editing = null },
        ) { text ->
            val i = existing?.let { e -> comments.indexOfFirst { it.id == e.id } } ?: -1
            when {
                i >= 0 && text.isEmpty() -> comments.removeAt(i)
                i >= 0 -> comments[i] = comments[i].copy(text = text)
                text.isNotEmpty() -> comments += ReviewComment(file.path, line, text)
            }
            editing = null
        }
    }
}

@Composable
private fun CommentEditor(place: String, code: String, initial: String, onDismiss: () -> Unit, done: (String) -> Unit) {
    var text by remember { mutableStateOf(initial) }
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (initial.isEmpty()) "Comment" else "Edit comment") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(place, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Text(
                    code, style = diffTextStyle(), maxLines = 3, overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainerHighest, RoundedCornerShape(8.dp)).padding(8.dp),
                )
                OutlinedTextField(
                    value = text, onValueChange = { text = it },
                    placeholder = { Text("What should change here?") },
                    minLines = 3, maxLines = 8,
                    modifier = Modifier.fillMaxWidth().focusRequester(focus),
                )
            }
        },
        confirmButton = {
            // Saving an emptied comment removes it.
            TextButton(onClick = { done(text.trim()) }, enabled = initial.isNotEmpty() || text.isNotBlank()) {
                Text(if (initial.isEmpty()) "Add" else "Save")
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
