package com.christoffermattssonlangseth.styrman.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.automirrored.filled.NoteAdd
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.FileDiff

/** "+3 −1" in green and red. */
@Composable
fun DiffCounts(diffs: List<FileDiff>) {
    val added = diffs.sumOf { it.added }
    val removed = diffs.sumOf { it.removed }
    Row(
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.semantics { contentDescription = "$added lines added, $removed removed" },
    ) {
        val style = MaterialTheme.typography.labelSmall.copy(fontFamily = Mono, fontWeight = FontWeight.SemiBold)
        if (added > 0) Text("+$added", style = style, color = diffAddedColor())
        if (removed > 0) Text("−$removed", style = style, color = diffRemovedColor())
    }
}

fun fileIcon(f: FileDiff) = when {
    f.isNew -> Icons.AutoMirrored.Filled.NoteAdd
    f.isDeleted -> Icons.Filled.Delete
    else -> Icons.Filled.Description
}

/** The diffs, one block per file. `maxLines` folds long ones behind "Show all". */
@Composable
fun DiffPreview(diffs: List<FileDiff>, cwd: String? = null, maxLines: Int? = null) {
    var showAll by remember { mutableStateOf(false) }
    val total = diffs.sumOf { it.lines.size }
    val hidden = maxLines?.let { maxOf(0, total - it) } ?: 0
    // The files, cut down to `maxLines` lines in all unless expanded (with the whole file
    // kept alongside, for its counts).
    val shown: List<Pair<FileDiff, FileDiff>> = if (maxLines == null || showAll) diffs.map { it to it } else {
        var left = maxLines
        diffs.mapNotNull { full ->
            if (left <= 0 && full.lines.isNotEmpty()) return@mapNotNull null
            val cut = full.copy(lines = full.lines.take(left))
            left -= cut.lines.size
            cut to full
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        shown.forEach { (f, full) -> FileBlock(f, full, cwd) }
        if (hidden > 0) TextButton(onClick = { showAll = !showAll }) {
            Text(if (showAll) "Show less" else "Show all $total lines", style = MaterialTheme.typography.labelMedium)
        }
    }
}

@Composable
private fun FileBlock(f: FileDiff, full: FileDiff, cwd: String?) {
    Column(Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surfaceContainerHighest, RoundedCornerShape(8.dp))) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            val muted = MaterialTheme.colorScheme.onSurfaceVariant
            Icon(fileIcon(f), null, tint = muted, modifier = Modifier.size(14.dp))
            Text(relativePath(f.path, cwd), Modifier.weight(1f), maxLines = 1,
                overflow = TextOverflow.StartEllipsis, style = MaterialTheme.typography.labelMedium, color = muted)
            if (f.isNew) Text("new", style = MaterialTheme.typography.labelMedium, color = diffAddedColor())
            if (f.isDeleted) Text("deleted", style = MaterialTheme.typography.labelMedium, color = diffRemovedColor())
            DiffCounts(listOf(full))
        }
        if (f.lines.isNotEmpty()) DiffLines(f.lines)
    }
}

/** Lines side by side in a horizontally scrolling block, all as wide as the widest. */
@Composable
private fun DiffLines(lines: List<FileDiff.Line>) {
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val width = diffContentWidth(lines, minWidth = maxWidth, extra = 36.dp)
        Column(Modifier.horizontalScroll(rememberScrollState()).width(width).padding(bottom = 4.dp)) {
            lines.forEach { DiffLineRow(it, numbers = false) }
        }
    }
}

/** One line of a diff: sign, optional line number, text, on a green / red wash. */
@Composable
fun DiffLineRow(l: FileDiff.Line, numbers: Boolean, onClick: (() -> Unit)? = null) {
    val faint = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.6f)
    if (l.kind == FileDiff.Kind.Gap) {
        Text(l.text.ifEmpty { "⋯" }, style = MaterialTheme.typography.labelSmall, color = faint,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 2.dp))
        return
    }
    val added = diffAddedColor()
    val removed = diffRemovedColor()
    val wash = when (l.kind) {
        FileDiff.Kind.Added -> added.copy(alpha = 0.14f)
        FileDiff.Kind.Removed -> removed.copy(alpha = 0.14f)
        else -> Color.Transparent
    }
    Row(
        Modifier.fillMaxWidth().background(wash)
            .then(if (onClick != null) Modifier.clickable(onClickLabel = "Comment on this line", onClick = onClick) else Modifier)
            .padding(horizontal = 8.dp, vertical = 1.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (numbers) Text((l.new ?: l.old)?.toString() ?: "", style = diffTextStyle(), color = faint,
            modifier = Modifier.width(36.dp), textAlign = androidx.compose.ui.text.style.TextAlign.End)
        Text(
            when (l.kind) { FileDiff.Kind.Added -> "+"; FileDiff.Kind.Removed -> "−"; else -> " " },
            style = diffTextStyle(), color = if (l.kind == FileDiff.Kind.Added) added else removed,
            modifier = Modifier.width(8.dp),
        )
        Text(
            displayLine(l.text), style = diffTextStyle(), softWrap = false, maxLines = 1,
            color = if (l.kind == FileDiff.Kind.Context) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
        )
    }
}

@Composable
fun diffTextStyle(): TextStyle = MaterialTheme.typography.bodySmall.copy(fontFamily = Mono)

/** Tabs as spaces, so widths measure right; empty lines keep their height. */
fun displayLine(s: String) = s.replace("\t", "    ").ifEmpty { " " }

/** How wide the widest line is (monospaced), at least `minWidth`. `extra`: room for the gutter. */
@Composable
fun diffContentWidth(lines: List<FileDiff.Line>, minWidth: Dp, extra: Dp): Dp {
    val measurer = rememberTextMeasurer()
    val style = diffTextStyle()
    val density = LocalDensity.current
    val charWidth = remember(style, density) { with(density) { measurer.measure("0", style).size.width.toDp() } }
    val longest = remember(lines) { lines.maxOfOrNull { displayLine(it.text).length } ?: 0 }
    return maxOf(minWidth, charWidth * longest + extra + 24.dp)
}

/** A path relative to the session folder, when it's inside it. */
fun relativePath(p: String, cwd: String?): String {
    if (cwd.isNullOrEmpty()) return p
    val base = if (cwd.endsWith("/")) cwd else "$cwd/"
    return if (p.startsWith(base)) p.removePrefix(base) else p
}
