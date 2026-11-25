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

/** A session's timeline, built from its events (same rules as the iOS app). */
class SessionTimeline(val sessionId: String) {
    val items = mutableStateListOf<TimelineItem>()
    var lastSeq = 0L
        private set
    /** History caught up and live events flowing. */
    var isLive by mutableStateOf(false)
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

    val hasContent get() = items.any { it !is TimelineItem.Divider }

    fun apply(events: List<Event>) {
        for (e in events) if (e.seq > lastSeq) {
            apply(e)
            lastSeq = e.seq
        }
    }

    private fun apply(e: Event) {
        val key = e.seq.toString()
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
            EventKind.TurnStarted, EventKind.TurnEnded, is EventKind.Unknown -> {}
        }
    }

    companion object {
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
