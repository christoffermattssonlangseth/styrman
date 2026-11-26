// Colored diffs for file edits (port of the iOS app's DiffView.swift): built from what
// the agent asked to change (Claude's Edit / MultiEdit / Write input, Codex's
// apply_patch), and from `git diff` text for the Changes view.

package com.christoffermattssonlangseth.styrman

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlin.math.abs

data class FileDiff(
    val path: String,
    val lines: List<Line>,
    /** A new file (Write, or Codex's "Add File"). */
    val isNew: Boolean = false,
    val isDeleted: Boolean = false,
) {
    enum class Kind { Added, Removed, Context, Gap }

    /** `old` / `new`: line numbers in each file, when the diff says (git's hunks). */
    data class Line(val kind: Kind, val text: String, val old: Int? = null, val new: Int? = null)

    val added get() = lines.count { it.kind == Kind.Added }
    val removed get() = lines.count { it.kind == Kind.Removed }

    companion object {
        /** The diffs an edit tool's input describes, or null when it isn't one we can read. */
        fun from(tool: ToolInfo): List<FileDiff>? {
            val input = tool.input
            val o = input as? JsonObject
            fun field(k: String) = o?.get(k).string()
            val path = field("file_path") ?: field("path") ?: tool.locations.firstOrNull() ?: "file"
            // Codex: apply_patch carries a patch (as `patch`, `input`, `command`, or the whole input).
            val patch = field("patch") ?: field("input") ?: field("command") ?: input.string()
            if (patch != null && ("*** Begin Patch" in patch || "*** Update File:" in patch || "*** Add File:" in patch)) {
                return codexPatch(patch).ifEmpty { null }
            }
            // Claude: Edit, and MultiEdit's list of edits.
            val old = field("old_string")
            val new = field("new_string")
            if (old != null && new != null) return listOf(FileDiff(path, lines(old, new)))
            (o?.get("edits") as? JsonArray)?.let { edits ->
                val parts = edits.mapNotNull { e ->
                    val eo = e as? JsonObject ?: return@mapNotNull null
                    val a = eo["old_string"].string() ?: return@mapNotNull null
                    val b = eo["new_string"].string() ?: return@mapNotNull null
                    lines(a, b)
                }
                if (parts.isEmpty()) return null
                val joined = parts.flatMapIndexed { i, p -> if (i == 0) p else listOf(Line(Kind.Gap, "")) + p }
                return listOf(FileDiff(path, joined))
            }
            // Claude: Write (the whole file, new or replaced).
            val content = field("content")
            if (tool.name == "Write" && content != null) {
                return listOf(FileDiff(path, split(content).map { Line(Kind.Added, it) }, isNew = true))
            }
            // A unified diff, if an agent hands one over.
            val d = field("diff") ?: field("patch")
            if (d != null && ("\n+" in d || "\n-" in d)) return listOf(FileDiff(path, unified(d)))
            return null
        }

        /** The line-level difference between two snippets: shared lines as context. */
        fun lines(old: String, new: String): List<Line> {
            val a = split(old)
            val b = split(new)
            // Snippets are small; past this, list the old lines then the new ones.
            if (a.size.toLong() * b.size > 160_000) {
                return a.map { Line(Kind.Removed, it) } + b.map { Line(Kind.Added, it) }
            }
            // Longest common subsequence, then walk it.
            val lcs = Array(a.size + 1) { IntArray(b.size + 1) }
            for (i in a.indices.reversed()) for (j in b.indices.reversed()) {
                lcs[i][j] = if (a[i] == b[j]) lcs[i + 1][j + 1] + 1 else maxOf(lcs[i + 1][j], lcs[i][j + 1])
            }
            val out = ArrayList<Line>()
            var i = 0
            var j = 0
            while (i < a.size || j < b.size) {
                if (i < a.size && j < b.size && a[i] == b[j]) {
                    out += Line(Kind.Context, a[i]); i++; j++
                } else if (i < a.size && (j == b.size || lcs[i + 1][j] >= lcs[i][j + 1])) {
                    out += Line(Kind.Removed, a[i]); i++
                } else {
                    out += Line(Kind.Added, b[j]); j++
                }
            }
            return trimContext(out)
        }

        /** Codex's patch format: "*** Update File: path", then +/-/space lines and "@@" hunks. */
        fun codexPatch(patch: String): List<FileDiff> {
            val files = ArrayList<FileDiff>()
            var current: FileDiff? = null
            fun flush() { current?.let { files += it }; current = null }
            fun add(l: Line) { current = current?.let { it.copy(lines = it.lines + l) } }
            for (l in patch.split("\n")) {
                when {
                    l.startsWith("*** Update File: ") -> { flush(); current = FileDiff(l.after("*** Update File: "), emptyList()) }
                    l.startsWith("*** Add File: ") -> { flush(); current = FileDiff(l.after("*** Add File: "), emptyList(), isNew = true) }
                    l.startsWith("*** Delete File: ") -> { flush(); files += FileDiff(l.after("*** Delete File: "), emptyList(), isDeleted = true) }
                    l.startsWith("*** Move to: ") -> current = current?.copy(path = l.after("*** Move to: "))
                    l.startsWith("*** ") -> {} // Begin / End Patch, End of File
                    l.startsWith("@@") -> if (current?.lines?.isNotEmpty() == true) add(Line(Kind.Gap, ""))
                    l.startsWith("+") -> add(Line(Kind.Added, l.substring(1)))
                    l.startsWith("-") -> add(Line(Kind.Removed, l.substring(1)))
                    l.startsWith(" ") -> add(Line(Kind.Context, l.substring(1)))
                }
            }
            flush()
            return files
        }

        /** A unified diff's body (headers dropped). */
        fun unified(d: String): List<Line> = d.split("\n").mapNotNull { l ->
            when {
                l.startsWith("+++") || l.startsWith("---") || l.startsWith("diff ") || l.startsWith("index ") -> null
                l.startsWith("@@") -> Line(Kind.Gap, "")
                l.startsWith("+") -> Line(Kind.Added, l.substring(1))
                l.startsWith("-") -> Line(Kind.Removed, l.substring(1))
                l.startsWith(" ") -> Line(Kind.Context, l.substring(1))
                else -> null
            }
        }

        private fun split(s: String): List<String> {
            val lines = s.split("\n")
            return if (lines.last().isEmpty()) lines.dropLast(1) else lines
        }

        /** Long runs of unchanged lines shrink to two lines either side of a change. */
        private fun trimContext(lines: List<Line>, keep: Int = 2): List<Line> {
            val changed = lines.indices.filter { lines[it].kind != Kind.Context }
            if (changed.isEmpty()) return lines
            val out = ArrayList<Line>()
            var lastKept = -1
            for (i in lines.indices) {
                if (lines[i].kind != Kind.Context || changed.any { abs(it - i) <= keep }) {
                    if (lastKept >= 0 && i - lastKept > 1) out += Line(Kind.Gap, "")
                    out += lines[i]
                    lastKept = i
                }
            }
            return out
        }

        private fun String.after(prefix: String) = removePrefix(prefix).trim()
    }
}

private fun JsonElement?.string(): String? = (this as? JsonPrimitive)?.takeIf { it.isString }?.content

/** A repository's uncommitted changes (`git diff` text) as one FileDiff per file, with line numbers. */
object GitChanges {
    fun files(diff: String): List<FileDiff> {
        val files = ArrayList<FileDiff>()
        var path: String? = null
        var isNew = false
        var isDeleted = false
        var lines = ArrayList<FileDiff.Line>()
        var old = 0
        var new = 0
        fun flush() {
            path?.let { files += FileDiff(it, lines, isNew, isDeleted) }
            path = null; isNew = false; isDeleted = false; lines = ArrayList()
        }
        for (l in diff.split("\n")) {
            when {
                l.startsWith("diff --git ") -> {
                    flush()
                    // "diff --git a/path b/path": the b/ side (renames end up there).
                    val b = l.lastIndexOf(" b/")
                    path = if (b >= 0) l.substring(b + 3) else l
                }
                l.startsWith("new file mode") -> isNew = true
                l.startsWith("deleted file mode") -> isDeleted = true
                listOf("+++ ", "--- ", "index ", "similarity ", "rename ", "old mode", "new mode").any { l.startsWith(it) } -> {}
                l.startsWith("@@") -> {
                    // "@@ -12,5 +12,7 @@ fn x": where each side starts.
                    val parts = l.split(" ")
                    val o = parts.getOrNull(1)?.drop(1)?.substringBefore(',')?.toIntOrNull()
                    val n = parts.getOrNull(2)?.drop(1)?.substringBefore(',')?.toIntOrNull()
                    if (o != null && n != null) {
                        old = o; new = n
                        if (path != null && lines.isNotEmpty()) lines += FileDiff.Line(FileDiff.Kind.Gap, "")
                    } else if (path != null) {
                        // "@@ binary file: not shown @@" and the like.
                        lines += FileDiff.Line(FileDiff.Kind.Gap, l.trim('@', ' '))
                    }
                }
                path == null -> {}
                l.startsWith("+") -> { lines += FileDiff.Line(FileDiff.Kind.Added, l.substring(1), new = new); new++ }
                l.startsWith("-") -> { lines += FileDiff.Line(FileDiff.Kind.Removed, l.substring(1), old = old); old++ }
                l.startsWith(" ") -> { lines += FileDiff.Line(FileDiff.Kind.Context, l.substring(1), old = old, new = new); old++; new++ }
            }
        }
        flush()
        return files
    }
}

/** A comment on one line of a change, for the agent. */
data class ReviewComment(
    val path: String,
    val line: FileDiff.Line,
    val text: String,
    val id: String = java.util.UUID.randomUUID().toString(),
) {
    /** "src/upload.rs line 42" (the new file's line; a removed line says so). */
    val place: String
        get() = when {
            line.new != null -> "`$path` line ${line.new}"
            line.old != null -> "`$path` (removed line ${line.old})"
            else -> "`$path`"
        }
}

object ReviewMessage {
    /** All comments as one message for the agent (same text as the iOS app sends). */
    fun compose(comments: List<ReviewComment>): String {
        val out = StringBuilder("Review comments on the uncommitted changes:\n")
        comments.forEachIndexed { i, c ->
            val code = c.line.text.trim(' ', '\t')
            out.append("\n${i + 1}. ${c.place}")
            if (code.isNotEmpty()) out.append(": `${code.take(120)}`")
            out.append("\n   ${c.text.replace("\n", "\n   ")}\n")
        }
        return out.toString()
    }
}
