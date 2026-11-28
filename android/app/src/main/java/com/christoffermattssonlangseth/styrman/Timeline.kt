package com.christoffermattssonlangseth.styrman

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** One row in a session's timeline. */
sealed interface TimelineItem {
    val id: String

    data class Divider(override val id: String, val text: String, val atMs: Long) : TimelineItem
    data class User(override val id: String, val text: String, val fromPhone: Boolean) : TimelineItem
    data class Assistant(override val id: String, val text: String) : TimelineItem
    data class Tool(override val id: String, val call: String, val tool: ToolInfo, val status: ToolStatus, val output: String?) : TimelineItem
    data class Permission(
        override val id: String,
        val request: String,
        val tool: ToolInfo,
        val reason: String?,
        val options: List<PermissionOption>,
        /** null while open; otherwise how it ended ("Allowed on this phone", …). */
        val resolution: String? = null,
    ) : TimelineItem {
        val isOpen get() = resolution == null
    }
    data class Plan(override val id: String, val entries: List<PlanEntry>) : TimelineItem
    data class Notice(override val id: String, val text: String, val warning: Boolean) : TimelineItem
}

/** A shell command or agent the agent left running in the background, from its start
 *  until Claude Code says it's done. */
data class AgentTask(
    /** The tool call. */
    val id: String,
    val tool: ToolInfo,
    val startedMs: Long,
    val ended: Ended? = null,
) {
    /** When and how it ended: "completed", "failed", "killed", "ended", … */
    data class Ended(val atMs: Long, val status: String, val summary: String?)

    val isAgent get() = tool.name != "Bash"
    val title: String
        get() {
            val o = tool.input as? kotlinx.serialization.json.JsonObject
            return o?.str("description") ?: o?.str("command") ?: tool.title
        }
}

/** A session's timeline, built from its events (same rules as the iOS app). */
class SessionTimeline(val sessionId: String) {
    val items = mutableStateListOf<TimelineItem>()
    var lastSeq = 0L
        private set
    private var liveState by mutableStateOf(false)
    /** History caught up and live events flowing. */
    var isLive: Boolean
        get() = liveState
        set(value) {
            liveState = value
            // Offline, nothing keeps it current.
            if (!value) { preview = null; status = null }
        }

    /** The agent's status line while it works, and when it was read (its clock runs
     *  on from there). */
    var status: WorkStatus? by mutableStateOf(null)
        private set
    var statusAtMs = 0L
        private set

    /** Since when it's been thinking (for "still thinking…"). */
    var thinkingSinceMs: Long? = null
        private set

    fun showStatus(s: WorkStatus?) {
        if (s == status) return
        thinkingSinceMs = if (s?.thinking == true) thinkingSinceMs.takeIf { status?.thinking == true } ?: System.currentTimeMillis() else null
        statusAtMs = System.currentTimeMillis()
        status = s
    }

    /** The reply the agent is writing right now, as its terminal shows it. Gone when
     *  the real message (or anything else from the agent) arrives. */
    var preview: String? by mutableStateOf(null)
        private set

    fun showPreview(text: String?) {
        if (text.isNullOrEmpty()) { preview = null; return }
        // Already here as a message (the screen redrew a finished reply): no echo.
        preview = if (alreadySaid(text, items.takeLast(12))) null else text
    }

    /** The diffs a permission is about: from its own input, or else from the latest edit
     *  of the same file (Codex asks with the paths only). */
    fun diffs(permission: TimelineItem.Permission): List<FileDiff>? {
        if (permission.tool.kind != ToolKind.Edit) return null
        FileDiff.from(permission.tool)?.let { return it }
        val paths = permission.tool.locations.toSet() +
            listOfNotNull(((permission.tool.input as? kotlinx.serialization.json.JsonObject)?.str("path")))
        for (item in items.asReversed()) {
            val t = item as? TimelineItem.Tool ?: continue
            if (t.tool.kind != ToolKind.Edit) continue
            val d = FileDiff.from(t.tool) ?: continue
            val mine = d.map { it.path }.toSet() + t.tool.locations
            if (paths.isEmpty() || mine.any { m -> paths.any { p -> p == m || p.endsWith(m) || m.endsWith(p) } }) return d
        }
        return null
    }
    /** Prompts sent from this phone that the agent hasn't picked up yet. */
    val pending = mutableStateListOf<PendingPrompt>()

    data class PendingPrompt(
        val text: String,
        val id: String = java.util.UUID.randomUUID().toString(),
        val queued: Boolean = false,
        val delivered: Boolean = false,
        val unconfirmed: Boolean = false,
    )

    private val toolIndex = HashMap<String, Int>()
    private val permissionIndex = HashMap<String, Int>()
    private val messageIndex = HashMap<String, Int>()
    private var planIndex: Int? = null

    fun addPending(text: String): String {
        val prompt = PendingPrompt(text)
        pending.add(prompt)
        return prompt.id
    }

    fun markQueued(text: String) = update({ it.text == text && !it.queued && !it.delivered }) {
        it.copy(queued = true, unconfirmed = false)
    }

    fun markDelivered(text: String) = update({ it.text == text && !it.delivered }) {
        it.copy(delivered = true, unconfirmed = false)
    }

    fun markUnconfirmed(id: String) = update({ it.id == id && !it.queued && !it.delivered }) {
        it.copy(unconfirmed = true)
    }

    fun dismissUnconfirmed(id: String) {
        pending.removeAll { it.id == id && it.unconfirmed }
    }

    fun dropPending(text: String) {
        val i = pending.indexOfFirst { it.text == text }
        if (i >= 0) pending.removeAt(i)
    }

    private fun update(matches: (PendingPrompt) -> Boolean, change: (PendingPrompt) -> PendingPrompt) {
        val i = pending.indexOfFirst(matches)
        if (i >= 0) pending[i] = change(pending[i])
    }

    /** Shell commands and agents left running in the background, oldest first. */
    val tasks = mutableStateListOf<AgentTask>()
    val runningTasks get() = tasks.filter { it.ended == null }

    private fun trackTask(e: Event) {
        when (val k = e.kind) {
            is EventKind.ToolCall -> {
                val bg = (k.tool.input as? kotlinx.serialization.json.JsonObject)?.bool("run_in_background") == true
                if (!bg || k.call.isEmpty() || e.atMs < TASKS_RECORDED_SINCE_MS) return
                val i = tasks.indexOfFirst { it.id == k.call }
                val failed = if (k.status == ToolStatus.Failed) AgentTask.Ended(e.atMs, "failed", null) else null
                if (i >= 0) tasks[i] = tasks[i].copy(tool = k.tool, ended = tasks[i].ended ?: failed)
                else tasks += AgentTask(k.call, k.tool, e.atMs, failed)
            }
            is EventKind.TaskEnded -> {
                val i = tasks.indexOfFirst { it.id == k.call && it.ended == null }
                if (i >= 0) tasks[i] = tasks[i].copy(ended = AgentTask.Ended(e.atMs, k.status, k.summary))
            }
            is EventKind.SessionEnded -> endAllTasks(e.atMs)
            // Claude Code's background tasks don't outlive it (they do outlive a
            // compaction or /clear, which start anew in the same process).
            is EventKind.SessionStarted -> if (k.start == "new" || k.start == "resume") endAllTasks(e.atMs)
            else -> {}
        }
    }

    private fun endAllTasks(atMs: Long) {
        for (i in tasks.indices) if (tasks[i].ended == null) tasks[i] = tasks[i].copy(ended = AgentTask.Ended(atMs, "ended", null))
    }

    val hasContent get() = items.any { it !is TimelineItem.Divider }

    fun apply(events: List<Event>) {
        for (e in events) if (e.seq > lastSeq) {
            apply(e)
            lastSeq = e.seq
        }
    }

    private fun apply(e: Event) {
        trackTask(e)
        val key = e.seq.toString()
        when (e.kind) {
            is EventKind.AssistantMessage, is EventKind.UserMessage, is EventKind.ToolCall,
            is EventKind.PermissionRequested, EventKind.TurnEnded, is EventKind.SessionEnded -> preview = null
            else -> {}
        }
        if (e.kind == EventKind.TurnEnded || e.kind is EventKind.SessionEnded) status = null
        when (val k = e.kind) {
            is EventKind.SessionStarted ->
                items += TimelineItem.Divider(key, if (k.start == "resume") "Session resumed" else "Session started", e.atMs)
            is EventKind.SessionEnded -> items += TimelineItem.Divider(key, "Session ended", e.atMs)
            is EventKind.SharingChanged ->
                items += TimelineItem.Divider(key, if (k.shared) "Shared with this phone" else "Sharing stopped", e.atMs)
            is EventKind.UserMessage -> {
                dropPending(k.text)
                items += TimelineItem.User(key, k.text, k.fromPhone)
            }
            is EventKind.AssistantMessage -> {
                val i = messageIndex[k.id]
                val old = i?.let { items[it] as? TimelineItem.Assistant }
                if (i != null && old != null) {
                    items[i] = old.copy(text = if (k.delta) old.text + k.text else k.text)
                } else {
                    messageIndex[k.id] = items.size
                    items += TimelineItem.Assistant(key, k.text)
                }
            }
            is EventKind.ToolCall -> {
                val i = if (k.call.isNotEmpty()) toolIndex[k.call] else null
                val old = i?.let { items[it] as? TimelineItem.Tool }
                if (i != null && old != null) {
                    items[i] = old.copy(tool = k.tool, status = k.status, output = k.output ?: old.output)
                } else {
                    if (k.call.isNotEmpty()) toolIndex[k.call] = items.size
                    items += TimelineItem.Tool(key, k.call, k.tool, k.status, k.output)
                }
            }
            is EventKind.PermissionRequested -> {
                permissionIndex[k.request] = items.size
                items += TimelineItem.Permission(key, k.request, k.tool, k.reason, k.options)
            }
            is EventKind.PermissionResolved -> {
                val i = permissionIndex[k.request] ?: return
                val p = items[i] as? TimelineItem.Permission ?: return
                items[i] = p.copy(resolution = resolutionText(k))
            }
            is EventKind.Plan -> {
                val i = planIndex
                if (i != null && items[i] is TimelineItem.Plan) {
                    items[i] = TimelineItem.Plan(items[i].id, k.entries)
                } else {
                    planIndex = items.size
                    items += TimelineItem.Plan(key, k.entries)
                }
            }
            is EventKind.Notice -> items += TimelineItem.Notice(key, k.text, k.warning)
            is EventKind.Error -> items += TimelineItem.Notice(key, k.message, true)
            EventKind.TurnStarted, EventKind.TurnEnded, is EventKind.Unknown, is EventKind.TaskEnded -> {}
        }
    }

    companion object {
        /** When the Mac began recording when background tasks end (`task_ended`): tasks
         *  started earlier have no ending on record and would look running forever. */
        const val TASKS_RECORDED_SINCE_MS = 1_795_816_657_000L

        /** Whether [preview] is a reply already among [items]. The preview is read off
         *  the terminal (a heading as bold, code without backticks) and the message is
         *  Markdown: compare the words, not the markup. */
        fun alreadySaid(preview: String, items: List<TimelineItem>): Boolean {
            fun words(s: String) = s.filterNot { it in "*`#>" }.split(Regex("\\s+")).filter { it.isNotEmpty() }.joinToString(" ")
            val body = words(preview.removePrefix("…")).take(200)
            if (body.isEmpty()) return false
            return items.any { it is TimelineItem.Assistant && words(it.text).contains(body) }
        }

        fun resolutionText(k: EventKind.PermissionResolved): String {
            val where = when (k.by) {
                "phone" -> "on a phone"
                "terminal" -> "in the terminal"
                else -> ""
            }
            return when (k.outcome) {
                "selected" -> (if (k.optionKind?.isAllow == true) "Allowed" else "Denied") + if (where.isEmpty()) "" else " $where"
                "timed_out" -> "No answer in time; answer in the terminal"
                else -> if (k.by == "terminal") "Answered in the terminal" else "Closed"
            }
        }
    }
}
